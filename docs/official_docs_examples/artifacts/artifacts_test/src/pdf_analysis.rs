#![allow(clippy::collapsible_if)]
//! PDF Document Analysis Example
//!
//! Demonstrates using BeforeModelCallback to inject a PDF document
//! into the LLM request for document analysis. Gemini natively supports PDF.
//!
//! Run:
//!   cd doc-test/artifacts/artifacts_test
//!   GOOGLE_API_KEY=your_key cargo run --bin pdf_analysis

use adk_artifact::{ArtifactService, InMemoryArtifactService, LoadRequest, SaveRequest};
use adk_core::{BeforeModelResult, Part};
use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    println!("PDF Document Analysis Example");
    println!("==============================\n");

    // Load PDF from file
    let pdf_bytes = std::fs::read("document.pdf")?;
    println!("Loaded document.pdf ({} bytes)\n", pdf_bytes.len());

    // Save as artifact
    let artifact_service = Arc::new(InMemoryArtifactService::new());
    artifact_service
        .save(SaveRequest {
            app_name: "pdf_app".to_string(),
            user_id: "user".to_string(),
            session_id: "init".to_string(),
            file_name: "user:document.pdf".to_string(),
            part: Part::InlineData { data: pdf_bytes, mime_type: "application/pdf".to_string() },
            version: None,
        })
        .await?;

    let callback_service = artifact_service.clone();

    let agent = LlmAgentBuilder::new("pdf_analyst")
        .description("Analyzes PDF documents")
        .instruction("You are a document analyst. The user has provided a PDF document. Answer questions about its content, summarize sections, or extract specific information as requested.")
        .model(model)
        .before_model_callback(Box::new(move |_ctx, mut request| {
            let service = callback_service.clone();
            Box::pin(async move {
                if let Ok(response) = service
                    .load(LoadRequest {
                        app_name: "pdf_app".to_string(),
                        user_id: "user".to_string(),
                        session_id: "init".to_string(),
                        file_name: "user:document.pdf".to_string(),
                        version: None,
                    })
                    .await
                {
                    if let Some(last_content) = request.contents.last_mut() {
                        if last_content.role == "user" {
                            // Inject PDF as InlineData - Gemini processes it natively
                            last_content.parts.push(response.part);
                        }
                    }
                }
                Ok(BeforeModelResult::Continue(request))
            })
        }))
        .build()?;

    println!("Ask questions about the PDF document:\n");

    adk_cli::console::run_console(Arc::new(agent), "pdf_demo".to_string(), "user".to_string())
        .await?;

    Ok(())
}
