//! Multimodal Function Responses — returning images from tools.
//!
//! Demonstrates the multimodal function response capability where a tool
//! returns binary data (a PNG image) alongside a JSON payload. The Gemini model
//! receives both the structured data and the image inside the `functionResponse`
//! wire object, then reasons over the visual content in its reply.
//!
//! Tools return multimodal data by including `inline_data` and/or `file_data`
//! arrays in their JSON return value. The framework automatically extracts these
//! into `FunctionResponseData` fields via `from_tool_result()`, and the conversion
//! layer serializes them as nested `parts` inside the Gemini `functionResponse`.
//!
//! Scenarios:
//!   1. A chart tool that returns a PNG image alongside JSON metadata
//!   2. A document tool that returns a file reference (URI) alongside JSON
//!
//! # Usage
//!
//! ```bash
//! export GOOGLE_API_KEY=your-key-here
//! cargo run --manifest-path examples/multimodal_function_response/Cargo.toml
//! ```

use adk_core::{Content, Part, SessionId, UserId};
use adk_model::GeminiModel;
use adk_rust::futures::StreamExt;
use adk_rust::prelude::*;
use adk_rust::session::{CreateRequest, SessionService};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

const APP_NAME: &str = "multimodal-fn-response-example";
const DEFAULT_MODEL: &str = "gemini-3.5-flash-lite";

fn load_dotenv() {
    let mut dir = std::env::current_dir().ok();
    while let Some(d) = dir {
        let path = d.join(".env");
        if path.is_file() {
            let _ = dotenvy::from_path(path);
            return;
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
}

fn api_key() -> String {
    std::env::var("GOOGLE_API_KEY").expect("GOOGLE_API_KEY must be set")
}

fn model_name() -> String {
    std::env::var("GEMINI_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string())
}

/// Generate a minimal valid 1x1 red PNG image.
fn tiny_red_png() -> Vec<u8> {
    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
    let ihdr: [u8; 13] = [0,0,0,1, 0,0,0,1, 8, 2, 0, 0, 0];
    let ihdr_crc = crc32(b"IHDR", &ihdr);
    png.extend_from_slice(&13u32.to_be_bytes());
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&ihdr);
    png.extend_from_slice(&ihdr_crc.to_be_bytes());
    let scanline: [u8; 4] = [0x00, 0xFF, 0x00, 0x00];
    let mut zlib = vec![0x78, 0x01, 0x01];
    let len = scanline.len() as u16;
    zlib.extend_from_slice(&len.to_le_bytes());
    zlib.extend_from_slice(&(!len).to_le_bytes());
    zlib.extend_from_slice(&scanline);
    zlib.extend_from_slice(&adler32(&scanline).to_be_bytes());
    let idat_crc = crc32(b"IDAT", &zlib);
    png.extend_from_slice(&(zlib.len() as u32).to_be_bytes());
    png.extend_from_slice(b"IDAT");
    png.extend_from_slice(&zlib);
    png.extend_from_slice(&idat_crc.to_be_bytes());
    let iend_crc = crc32(b"IEND", &[]);
    png.extend_from_slice(&0u32.to_be_bytes());
    png.extend_from_slice(b"IEND");
    png.extend_from_slice(&iend_crc.to_be_bytes());
    png
}

fn crc32(chunk_type: &[u8], data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in chunk_type.iter().chain(data.iter()) {
        crc ^= b as u32;
        for _ in 0..8 { crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 }; }
    }
    crc ^ 0xFFFF_FFFF
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b): (u32, u32) = (1, 0);
    for &byte in data { a = (a + byte as u32) % 65521; b = (b + a) % 65521; }
    (b << 16) | a
}

// ---------------------------------------------------------------------------
// Tools — return JSON with inline_data / file_data for multimodal responses
// ---------------------------------------------------------------------------

/// Chart tool: returns a PNG image alongside JSON metadata.
///
/// The `inline_data` array in the return value is automatically extracted by
/// `FunctionResponseData::from_tool_result()` and sent to Gemini as nested
/// `inlineData` parts inside the `functionResponse` wire object.
async fn generate_chart(
    _ctx: Arc<dyn ToolContext>,
    args: serde_json::Value,
) -> Result<serde_json::Value> {
    let title = args["title"].as_str().unwrap_or("Untitled Chart");
    let png_bytes = tiny_red_png();
    println!("  📊 [generate_chart] created '{title}' ({} bytes PNG)", png_bytes.len());

    // Return JSON with inline_data — the framework picks this up automatically
    Ok(json!({
        "response": {
            "title": title,
            "chart_type": "bar",
            "data_points": 5,
            "description": "A bar chart showing quarterly sales figures"
        },
        "inline_data": [{
            "mime_type": "image/png",
            "data": png_bytes
        }]
    }))
}

/// Document tool: returns a file URI reference alongside JSON metadata.
async fn fetch_document(
    _ctx: Arc<dyn ToolContext>,
    args: serde_json::Value,
) -> Result<serde_json::Value> {
    let doc_id = args["document_id"].as_str().unwrap_or("report-2024");
    println!("  📄 [fetch_document] retrieved '{doc_id}'");

    Ok(json!({
        "response": {
            "document_id": doc_id,
            "title": "Q4 2024 Sales Report",
            "pages": 12,
            "status": "retrieved"
        },
        "file_data": [{
            "mime_type": "application/pdf",
            "file_uri": format!("gs://example-bucket/reports/{doc_id}.pdf")
        }]
    }))
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

async fn make_runner(agent: Arc<dyn Agent>, session_id: &str) -> anyhow::Result<Runner> {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP_NAME.into(),
            user_id: "user".into(),
            session_id: Some(session_id.into()),
            state: HashMap::new(),
        })
        .await?;
    Ok(Runner::builder()
        .app_name(APP_NAME)
        .agent(agent)
        .session_service(sessions)
        .build()?)
}

fn separator(title: &str) {
    println!("\n{}", "=".repeat(60));
    println!("  {title}");
    println!("{}\n", "=".repeat(60));
}

async fn run_scenario(
    name: &str,
    instruction: &str,
    tools: Vec<Arc<dyn Tool>>,
    prompt: &str,
) -> anyhow::Result<()> {
    let model = Arc::new(GeminiModel::new(api_key(), &model_name())?);
    let mut builder = LlmAgentBuilder::new(name).instruction(instruction).model(model);
    for tool in tools {
        builder = builder.tool(tool);
    }
    let agent = Arc::new(builder.build()?);
    let runner = make_runner(agent, name).await?;

    let mut stream = runner
        .run(
            UserId::new("user")?,
            SessionId::new(name)?,
            Content::new("user").with_text(prompt),
        )
        .await?;

    while let Some(event) = stream.next().await {
        let event = event?;
        // The complete event repeats what the partial chunks streamed.
        if event.llm_response.partial {
            continue;
        }
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::FunctionCall { name, args, .. } => {
                        println!("  → FunctionCall: {name}({args})");
                    }
                    Part::FunctionResponse { function_response, .. } => {
                        let n_inline = function_response.inline_data.len();
                        let n_file = function_response.file_data.len();
                        println!(
                            "  ← FunctionResponse: {} [json + {} inline + {} file refs]",
                            function_response.name, n_inline, n_file,
                        );
                        for (i, p) in function_response.inline_data.iter().enumerate() {
                            println!("    inline[{i}]: {} ({} bytes)", p.mime_type, p.data.len());
                        }
                        for (i, p) in function_response.file_data.iter().enumerate() {
                            println!("    file[{i}]: {} → {}", p.mime_type, p.file_uri);
                        }
                    }
                    Part::Text { text } if !text.trim().is_empty() => print!("{text}"),
                    Part::Thinking { .. } => println!("  💭 (thinking...)"),
                    _ => {}
                }
            }
        }
    }
    println!();
    Ok(())
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

async fn scenario_chart() -> anyhow::Result<()> {
    separator("Scenario 1: Chart tool → PNG + JSON");

    #[derive(schemars::JsonSchema, serde::Serialize)]
    struct GenerateChartArgs { title: String }

    run_scenario(
        "chart",
        "You are a data visualization assistant. Use generate_chart when asked to create a chart. Describe what you see in the returned image.",
        vec![Arc::new(
            FunctionTool::new("generate_chart", "Generate a chart image with metadata.", generate_chart)
                .with_parameters_schema::<GenerateChartArgs>(),
        )],
        "Create a bar chart showing Q4 sales figures and describe the chart.",
    ).await
}

async fn scenario_document() -> anyhow::Result<()> {
    separator("Scenario 2: Document tool → file URI + JSON");

    #[derive(schemars::JsonSchema, serde::Serialize)]
    struct FetchDocumentArgs { document_id: String }

    run_scenario(
        "document",
        "You are a document retrieval assistant. Use fetch_document to retrieve documents and summarize the metadata.",
        vec![Arc::new(
            FunctionTool::new("fetch_document", "Retrieve a document by ID.", fetch_document)
                .with_parameters_schema::<FetchDocumentArgs>(),
        )],
        "Fetch the Q4 2024 sales report (document ID: report-q4-2024) and tell me about it.",
    ).await
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_dotenv();

    println!("Multimodal Function Responses — Example");
    println!("========================================");
    println!("Model: {}\n", model_name());

    if let Err(e) = scenario_chart().await {
        eprintln!("✗ Scenario 1 failed: {e:#}");
    }
    if let Err(e) = scenario_document().await {
        eprintln!("✗ Scenario 2 failed: {e:#}");
    }

    println!("\nDone.");
    Ok(())
}
