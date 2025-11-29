//! PDF analysis example using BeforeModel callback pattern (adk-go style)
//!
//! This example demonstrates how to load PDF artifacts for document analysis.
//! Following the adk-go pattern, we use a BeforeModelCallback to inject
//! the PDF directly into the LLM request.
//!
//! Gemini 1.5/2.0 can process PDFs natively:
//! - Extract and analyze text content
//! - Answer questions about the document
//! - Summarize sections
//! - Process up to ~1000 pages
//! - OCR for scanned documents

use adk_rust::prelude::*;
use adk_rust::artifact::{ArtifactService, InMemoryArtifactService, SaveRequest, LoadRequest};
use adk_rust::Launcher;
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = init_env();
    // Use a model that supports PDF input
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-pro")?);

    // Create artifact service and save the PDF
    let artifact_service = Arc::new(InMemoryArtifactService::new());

    // Load the test PDF file
    let pdf_path = "examples/artifacts/Test_PDF.pdf";
    let pdf_content = std::fs::read(pdf_path)
        .expect("Failed to read Test_PDF.pdf - make sure it exists in examples/artifacts/");
    let pdf_size = pdf_content.len();
    println!("Loaded PDF: {} ({} bytes)", pdf_path, pdf_size);

    // Save the PDF as a user-scoped artifact
    artifact_service.save(SaveRequest {
        app_name: "pdf_analyst".to_string(),
        user_id: "user".to_string(),
        session_id: "init".to_string(),
        file_name: "user:document.pdf".to_string(),
        part: Part::InlineData {
            data: pdf_content,
            mime_type: "application/pdf".to_string(),
        },
        version: None,
    }).await?;

    // Clone artifact service for use in callback
    let callback_artifact_service = artifact_service.clone();

    let agent = LlmAgentBuilder::new("pdf_analyst")
        .description("Analyzes PDF documents using BeforeModel callback pattern")
        .instruction(
            "You are a PDF document analyst. A PDF document has been provided to you. \
             You can answer questions about its content, summarize sections, \
             extract information, and analyze the document structure. \
             Be specific and quote relevant text when answering questions."
        )
        .model(model)
        // Use BeforeModel callback to inject PDF into the request (adk-go pattern)
        .before_model_callback(Box::new(move |_ctx, mut request| {
            let artifact_service = callback_artifact_service.clone();
            Box::pin(async move {
                // Load the PDF artifact
                let load_result = artifact_service.load(LoadRequest {
                    app_name: "pdf_analyst".to_string(),
                    user_id: "user".to_string(),
                    session_id: "init".to_string(),
                    file_name: "user:document.pdf".to_string(),
                    version: None,
                }).await;

                if let Ok(response) = load_result {
                    // Inject the PDF part into the last user content
                    if let Some(last_content) = request.contents.last_mut() {
                        if last_content.role == "user" {
                            last_content.parts.push(response.part);
                        }
                    }
                }

                Ok(BeforeModelResult::Continue(request))
            })
        }))
        .build()?;

    if is_interactive_mode() {
        Launcher::new(Arc::new(agent))
            .with_artifact_service(artifact_service)
            .run()
            .await?;
    } else {
        print_validating("PDF Analysis Agent (BeforeModel callback pattern)");
        println!("✓ PDF file loaded into artifact service: {} bytes", pdf_size);
        println!("✓ Agent uses BeforeModel callback to inject PDF into LLM request");
        println!("✓ Gemini can extract text, answer questions, and summarize PDFs");
        print_success("chat_pdf");
        println!("\nTry: cargo run --example chat_pdf -- chat");
        println!("Ask: 'What is this document about?' or 'Summarize the main points'");
    }

    Ok(())
}
