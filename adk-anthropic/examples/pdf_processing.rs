//! PDF processing with the Anthropic Messages API.
//!
//! Demonstrates three ways to send PDFs to Claude:
//! 1. URL reference — simplest, for publicly hosted PDFs
//! 2. Base64-encoded — for local files or when URLs aren't available
//! 3. PDF with citations — verifiable references to specific pages
//!
//! Claude extracts text, analyzes charts/tables, and understands visual content.
//! Each page is processed as both text and image (1,500–3,000 tokens/page).
//!
//! Run: `ANTHROPIC_API_KEY=sk-... cargo run -p adk-anthropic --example pdf_processing`

use adk_anthropic::{
    Anthropic, Base64PdfSource, CitationsConfig, ContentBlock, DocumentBlock, DocumentSource,
    KnownModel, MessageCreateParams, MessageParam, MessageRole, TextBlock, TextCitation,
    UrlPdfSource,
};

const SAMPLE_PDF_URL: &str = "https://assets.anthropic.com/m/1cd9d098ac3e6467/original/Claude-3-Model-Card-October-Addendum.pdf";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();

    let client = Anthropic::new(None)?;

    // ── 1. URL-based PDF ─────────────────────────────────────────
    println!("=== 1. URL-Based PDF ===\n");

    let doc =
        DocumentBlock::new(DocumentSource::UrlPdf(UrlPdfSource::new(SAMPLE_PDF_URL.to_string())))
            .with_title("Claude 3 Model Card".to_string());

    let messages = vec![MessageParam::new_with_blocks(
        vec![
            ContentBlock::Document(doc),
            ContentBlock::Text(TextBlock::new(
                "What models are discussed in this document? List them briefly.",
            )),
        ],
        MessageRole::User,
    )];

    let r = client
        .send(MessageCreateParams::new(512, messages, KnownModel::ClaudeSonnet46.into()))
        .await?;

    print_response(&r.content);
    println!("Tokens: {} in / {} out\n", r.usage.input_tokens, r.usage.output_tokens);

    // ── 2. Base64-encoded PDF ────────────────────────────────────
    println!("=== 2. Base64-Encoded PDF ===\n");

    // Create a minimal valid PDF in memory for demonstration.
    // In production you'd read a file: std::fs::read("report.pdf")?
    let pdf_bytes = build_minimal_pdf();
    let pdf_b64 = base64_encode(&pdf_bytes);

    let doc = DocumentBlock::new(DocumentSource::Base64Pdf(Base64PdfSource::new(pdf_b64)))
        .with_title("Sample Report".to_string());

    let messages = vec![MessageParam::new_with_blocks(
        vec![
            ContentBlock::Document(doc),
            ContentBlock::Text(TextBlock::new("What does this PDF contain?")),
        ],
        MessageRole::User,
    )];

    let r = client
        .send(MessageCreateParams::new(256, messages, KnownModel::ClaudeSonnet46.into()))
        .await?;

    print_response(&r.content);
    println!("Tokens: {} in / {} out\n", r.usage.input_tokens, r.usage.output_tokens);

    // ── 3. PDF with citations ────────────────────────────────────
    println!("=== 3. PDF with Citations ===\n");

    let doc =
        DocumentBlock::new(DocumentSource::UrlPdf(UrlPdfSource::new(SAMPLE_PDF_URL.to_string())))
            .with_title("Claude 3 Model Card".to_string())
            .with_citations(CitationsConfig::enabled());

    let messages = vec![MessageParam::new_with_blocks(
        vec![
            ContentBlock::Document(doc),
            ContentBlock::Text(TextBlock::new(
                "What safety evaluations were performed? Cite specific sections.",
            )),
        ],
        MessageRole::User,
    )];

    let r = client
        .send(MessageCreateParams::new(1024, messages, KnownModel::ClaudeSonnet46.into()))
        .await?;

    for block in &r.content {
        if let Some(text) = block.as_text() {
            if text.has_citations() {
                print!("\x1b[1m{}\x1b[0m", text.text);
                for c in text.citations.as_ref().unwrap() {
                    match c {
                        TextCitation::PageLocation(p) => {
                            print!(
                                " \x1b[90m[p.{}-{}]\x1b[0m",
                                p.start_page_number, p.end_page_number
                            );
                        }
                        TextCitation::CharLocation(ch) => {
                            print!(
                                " \x1b[90m[chars:{}..{}]\x1b[0m",
                                ch.start_char_index, ch.end_char_index
                            );
                        }
                        _ => print!(" \x1b[90m[cited]\x1b[0m"),
                    }
                }
            } else {
                print!("{}", text.text);
            }
        }
    }
    println!("\n\nTokens: {} in / {} out", r.usage.input_tokens, r.usage.output_tokens);

    Ok(())
}

fn print_response(content: &[ContentBlock]) {
    for block in content {
        if let Some(text) = block.as_text() {
            println!("{}", text.text);
        }
    }
}

fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// Build a minimal valid PDF with one page of text.
/// In production, you'd just read a file from disk.
fn build_minimal_pdf() -> Vec<u8> {
    let content = b"1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n\
        2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n\
        3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>endobj\n\
        4 0 obj<</Length 44>>stream\nBT /F1 12 Tf 100 700 Td (Hello from PDF) Tj ET\nendstream\nendobj\n\
        5 0 obj<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>endobj\n";

    let xref_offset = content.len();
    let mut pdf = Vec::from(&b"%PDF-1.4\n"[..]);
    pdf.extend_from_slice(content);
    pdf.extend_from_slice(
        format!(
            "xref\n0 6\n0000000000 65535 f \n\
             0000000009 00000 n \n\
             0000000058 00000 n \n\
             0000000115 00000 n \n\
             0000000266 00000 n \n\
             0000000360 00000 n \n\
             trailer<</Size 6/Root 1 0 R>>\nstartxref\n{xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );
    pdf
}
