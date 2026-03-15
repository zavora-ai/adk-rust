//! Vision / Multimodal Validation Example
//!
//! Sends a real image to each configured provider and prints the model's description.
//! Tests both `InlineData` (base64 bytes) and `FileData` (image URL) paths.
//!
//! # Usage
//!
//! Set one or more API keys, then run:
//!
//! ```bash
//! # Gemini
//! GOOGLE_API_KEY=... cargo run --example vision_test --features vision-test
//!
//! # OpenAI
//! OPENAI_API_KEY=... cargo run --example vision_test --features vision-test
//!
//! # Anthropic
//! ANTHROPIC_API_KEY=... cargo run --example vision_test --features vision-test
//!
//! # Bedrock (uses AWS credentials from environment/profile)
//! AWS_REGION=us-east-1 cargo run --example vision_test --features vision-test
//!
//! # All at once
//! GOOGLE_API_KEY=... OPENAI_API_KEY=... ANTHROPIC_API_KEY=... \
//!   cargo run --example vision_test --features vision-test
//! ```

use adk_core::{Content, GenerateContentConfig, Llm, LlmRequest, Part};
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;

/// A minimal valid 8×8 red PNG image.
/// Large enough for vision APIs to accept (they reject 1×1 images).
fn small_red_png() -> Vec<u8> {
    // Build a valid 8×8 RGB PNG with all-red pixels
    let width: u32 = 8;
    let height: u32 = 8;

    let mut png = Vec::new();
    // PNG signature
    png.extend_from_slice(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);

    // IHDR chunk: 8×8, bit_depth=8, color_type=2 (RGB)
    let ihdr_data: [u8; 13] = [
        0x00, 0x00, 0x00, 0x08, // width = 8
        0x00, 0x00, 0x00, 0x08, // height = 8
        0x08, // bit depth = 8
        0x02, // color type = RGB
        0x00, // compression
        0x00, // filter
        0x00, // interlace
    ];
    write_png_chunk(&mut png, b"IHDR", &ihdr_data);

    // Build raw scanlines: each row = filter_byte(0) + width * 3 bytes (RGB)
    let row_bytes = 1 + (width as usize) * 3; // filter byte + RGB pixels
    let mut raw_data = vec![0u8; row_bytes * height as usize];
    for row in 0..height as usize {
        let offset = row * row_bytes;
        raw_data[offset] = 0x00; // filter: None
        for px in 0..width as usize {
            let px_offset = offset + 1 + px * 3;
            raw_data[px_offset] = 0xFF; // R
            // G and B stay 0
        }
    }

    // Compress with deflate (use miniz_oxide which is already a transitive dep)
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&raw_data, 6);
    write_png_chunk(&mut png, b"IDAT", &compressed);

    // IEND chunk
    write_png_chunk(&mut png, b"IEND", &[]);
    png
}

fn write_png_chunk(buf: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) {
    buf.extend_from_slice(&(data.len() as u32).to_be_bytes());
    buf.extend_from_slice(chunk_type);
    buf.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(chunk_type);
    crc_input.extend_from_slice(data);
    buf.extend_from_slice(&png_crc32(&crc_input).to_be_bytes());
}

fn png_crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// A publicly accessible test image URL.
/// Using a simple, reliable image from httpbin-style service.
const TEST_IMAGE_URL: &str =
    "https://www.google.com/images/branding/googlelogo/2x/googlelogo_color_272x92dp.png";

/// Send a multimodal request to a model and collect the full text response.
async fn run_vision_test(model: Arc<dyn Llm>, label: &str, content: Content) -> anyhow::Result<()> {
    let request = LlmRequest {
        model: model.name().to_string(),
        contents: vec![content],
        config: Some(GenerateContentConfig { max_output_tokens: Some(256), ..Default::default() }),
        tools: HashMap::new(),
    };

    println!("\n{}", "=".repeat(60));
    println!("  {label}");
    println!("  Model: {}", model.name());
    println!("{}", "=".repeat(60));

    let mut stream = model.generate_content(request, false).await?;
    let mut full_text = String::new();

    while let Some(result) = stream.next().await {
        match result {
            Ok(response) => {
                if let Some(content) = &response.content {
                    for part in &content.parts {
                        if let Some(text) = part.text() {
                            full_text.push_str(text);
                        }
                    }
                }
                if let Some(usage) = &response.usage_metadata {
                    println!(
                        "  Tokens — prompt: {}, completion: {}, total: {}",
                        usage.prompt_token_count,
                        usage.candidates_token_count,
                        usage.total_token_count
                    );
                }
            }
            Err(e) => {
                println!("  ERROR: {e}");
                return Err(e.into());
            }
        }
    }

    if full_text.is_empty() {
        println!("  (no text response)");
    } else {
        // Truncate long responses for readability
        let display = if full_text.len() > 500 {
            format!("{}...", &full_text[..500])
        } else {
            full_text.clone()
        };
        println!("  Response: {display}");
    }

    println!("  ✓ PASS");
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let image_bytes = small_red_png();
    let mut tested = 0u32;
    let mut passed = 0u32;
    let mut failed = 0u32;

    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║        ADK Vision / Multimodal Validation Suite         ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!();
    println!("Test image: 8×8 red PNG ({} bytes)", image_bytes.len());
    println!("Test URL:   {TEST_IMAGE_URL}");

    // ── Gemini ──────────────────────────────────────────────────
    if let Ok(api_key) =
        std::env::var("GOOGLE_API_KEY").or_else(|_| std::env::var("GEMINI_API_KEY"))
    {
        let model: Arc<dyn Llm> = Arc::new(adk_model::gemini::GeminiModel::new(
            &api_key,
            "gemini-3.1-flash-lite-preview",
        )?);

        // Test 1: InlineData (base64 image bytes)
        tested += 1;
        let content = Content {
            role: "user".to_string(),
            parts: vec![
                Part::Text { text: "Describe this image in one sentence.".to_string() },
                Part::InlineData { mime_type: "image/png".to_string(), data: image_bytes.clone() },
            ],
        };
        match run_vision_test(model.clone(), "Gemini — InlineData (PNG bytes)", content).await {
            Ok(()) => passed += 1,
            Err(e) => {
                eprintln!("  ✗ FAIL: {e}");
                failed += 1;
            }
        }

        // Test 2: FileData (image URL)
        tested += 1;
        let content = Content {
            role: "user".to_string(),
            parts: vec![
                Part::Text { text: "Describe this image in one sentence.".to_string() },
                Part::FileData {
                    mime_type: "image/png".to_string(),
                    file_uri: TEST_IMAGE_URL.to_string(),
                },
            ],
        };
        match run_vision_test(model, "Gemini — FileData (image URL)", content).await {
            Ok(()) => passed += 1,
            Err(e) => {
                eprintln!("  ✗ FAIL: {e}");
                failed += 1;
            }
        }
    } else {
        println!("\n⏭  Skipping Gemini (no GOOGLE_API_KEY / GEMINI_API_KEY)");
    }

    // ── OpenAI ──────────────────────────────────────────────────
    #[cfg(feature = "openai")]
    if let Ok(api_key) = std::env::var("OPENAI_API_KEY") {
        let config = adk_model::openai::OpenAIConfig::new(&api_key, "gpt-4o-mini");
        let model: Arc<dyn Llm> = Arc::new(adk_model::openai::OpenAIClient::new(config)?);

        // Test 3: InlineData (base64 image bytes)
        tested += 1;
        let content = Content {
            role: "user".to_string(),
            parts: vec![
                Part::Text { text: "Describe this image in one sentence.".to_string() },
                Part::InlineData { mime_type: "image/png".to_string(), data: image_bytes.clone() },
            ],
        };
        match run_vision_test(model.clone(), "OpenAI — InlineData (PNG bytes)", content).await {
            Ok(()) => passed += 1,
            Err(e) => {
                eprintln!("  ✗ FAIL: {e}");
                failed += 1;
            }
        }

        // Test 4: FileData (image URL) — should map to ImageUrl
        tested += 1;
        let content = Content {
            role: "user".to_string(),
            parts: vec![
                Part::Text { text: "Describe this image in one sentence.".to_string() },
                Part::FileData {
                    mime_type: "image/png".to_string(),
                    file_uri: TEST_IMAGE_URL.to_string(),
                },
            ],
        };
        match run_vision_test(model, "OpenAI — FileData (image URL)", content).await {
            Ok(()) => passed += 1,
            Err(e) => {
                eprintln!("  ✗ FAIL: {e}");
                failed += 1;
            }
        }
    } else {
        #[cfg(feature = "openai")]
        println!("\n⏭  Skipping OpenAI (no OPENAI_API_KEY)");
    }
    #[cfg(not(feature = "openai"))]
    println!("\n⏭  Skipping OpenAI (feature 'openai' not enabled)");

    // ── Anthropic ───────────────────────────────────────────────
    #[cfg(feature = "anthropic")]
    if let Ok(api_key) = std::env::var("ANTHROPIC_API_KEY") {
        let config = adk_model::anthropic::AnthropicConfig::new(&api_key, "claude-sonnet-4-6");
        let model: Arc<dyn Llm> = Arc::new(adk_model::anthropic::AnthropicClient::new(config)?);

        // Test 5: InlineData (base64 image bytes)
        tested += 1;
        let content = Content {
            role: "user".to_string(),
            parts: vec![
                Part::Text { text: "Describe this image in one sentence.".to_string() },
                Part::InlineData { mime_type: "image/png".to_string(), data: image_bytes.clone() },
            ],
        };
        match run_vision_test(model.clone(), "Anthropic — InlineData (PNG bytes)", content).await
        {
            Ok(()) => passed += 1,
            Err(e) => {
                eprintln!("  ✗ FAIL: {e}");
                failed += 1;
            }
        }

        // Test 6: FileData (image URL) — should map to UrlImageSource
        tested += 1;
        let content = Content {
            role: "user".to_string(),
            parts: vec![
                Part::Text { text: "Describe this image in one sentence.".to_string() },
                Part::FileData {
                    mime_type: "image/png".to_string(),
                    file_uri: TEST_IMAGE_URL.to_string(),
                },
            ],
        };
        match run_vision_test(model, "Anthropic — FileData (image URL)", content).await {
            Ok(()) => passed += 1,
            Err(e) => {
                eprintln!("  ✗ FAIL: {e}");
                failed += 1;
            }
        }
    } else {
        #[cfg(feature = "anthropic")]
        println!("\n⏭  Skipping Anthropic (no ANTHROPIC_API_KEY)");
    }
    #[cfg(not(feature = "anthropic"))]
    println!("\n⏭  Skipping Anthropic (feature 'anthropic' not enabled)");

    // ── Bedrock ─────────────────────────────────────────────────
    #[cfg(feature = "bedrock")]
    if std::env::var("AWS_REGION").is_ok() || std::env::var("AWS_DEFAULT_REGION").is_ok() {
        let region = std::env::var("AWS_REGION")
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|_| "us-east-1".to_string());
        let config =
            adk_model::bedrock::BedrockConfig::new(&region, "us.anthropic.claude-sonnet-4-6");
        let model: Arc<dyn Llm> = Arc::new(adk_model::bedrock::BedrockClient::new(config).await?);

        // Test 7: InlineData (base64 image bytes) — should map to ImageBlock
        tested += 1;
        let content = Content {
            role: "user".to_string(),
            parts: vec![
                Part::Text { text: "Describe this image in one sentence.".to_string() },
                Part::InlineData { mime_type: "image/png".to_string(), data: image_bytes.clone() },
            ],
        };
        match run_vision_test(model.clone(), "Bedrock — InlineData (PNG bytes)", content).await {
            Ok(()) => passed += 1,
            Err(e) => {
                eprintln!("  ✗ FAIL: {e}");
                failed += 1;
            }
        }

        // Test 8: FileData (image URL) — Bedrock maps to text reference
        // (Bedrock only supports S3 URIs natively, HTTP URLs become text)
        tested += 1;
        let content = Content {
            role: "user".to_string(),
            parts: vec![
                Part::Text {
                    text: "The following is a reference to an image. Acknowledge you see the reference.".to_string(),
                },
                Part::FileData {
                    mime_type: "image/png".to_string(),
                    file_uri: TEST_IMAGE_URL.to_string(),
                },
            ],
        };
        match run_vision_test(model, "Bedrock — FileData (URL → text ref)", content).await {
            Ok(()) => passed += 1,
            Err(e) => {
                eprintln!("  ✗ FAIL: {e}");
                failed += 1;
            }
        }
    } else {
        #[cfg(feature = "bedrock")]
        println!("\n⏭  Skipping Bedrock (no AWS_REGION / AWS_DEFAULT_REGION)");
    }
    #[cfg(not(feature = "bedrock"))]
    println!("\n⏭  Skipping Bedrock (feature 'bedrock' not enabled)");

    // ── Summary ─────────────────────────────────────────────────
    println!("\n{}", "─".repeat(60));
    println!("  Results: {passed}/{tested} passed, {failed} failed");
    if tested == 0 {
        println!("  No providers configured. Set at least one API key.");
        println!("  See: cargo run --example vision_test --features vision-test -- --help");
    }
    println!("{}", "─".repeat(60));

    if failed > 0 {
        std::process::exit(1);
    }

    Ok(())
}
