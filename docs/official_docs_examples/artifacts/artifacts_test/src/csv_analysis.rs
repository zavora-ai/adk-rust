#![allow(clippy::collapsible_if)]
//! CSV Data Analysis Example
//!
//! Demonstrates using BeforeModelCallback to inject CSV data
//! into the LLM request for data analysis.
//!
//! Run:
//!   cd doc-test/artifacts/artifacts_test
//!   GOOGLE_API_KEY=your_key cargo run --bin csv_analysis

use adk_artifact::{ArtifactService, InMemoryArtifactService, LoadRequest, SaveRequest};
use adk_core::{BeforeModelResult, Part};
use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    println!("CSV Data Analysis Example");
    println!("=========================\n");

    // Load CSV from file
    let csv_data = std::fs::read_to_string("sales.csv")?;
    println!("Loaded sales.csv:\n{}", csv_data);

    // Save as artifact
    let artifact_service = Arc::new(InMemoryArtifactService::new());
    artifact_service
        .save(SaveRequest {
            app_name: "csv_app".to_string(),
            user_id: "user".to_string(),
            session_id: "init".to_string(),
            file_name: "user:sales.csv".to_string(),
            part: Part::Text { text: csv_data },
            version: None,
        })
        .await?;

    let callback_service = artifact_service.clone();

    let agent = LlmAgentBuilder::new("data_analyst")
        .description("Analyzes CSV data")
        .instruction("You are a data analyst. The user has provided CSV sales data. Answer questions about the data with specific numbers and insights.")
        .model(model)
        .before_model_callback(Box::new(move |_ctx, mut request| {
            let service = callback_service.clone();
            Box::pin(async move {
                if let Ok(response) = service
                    .load(LoadRequest {
                        app_name: "csv_app".to_string(),
                        user_id: "user".to_string(),
                        session_id: "init".to_string(),
                        file_name: "user:sales.csv".to_string(),
                        version: None,
                    })
                    .await
                {
                    if let Some(last_content) = request.contents.last_mut() {
                        if last_content.role == "user" {
                            if let Part::Text { text } = &response.part {
                                last_content.parts.insert(
                                    0,
                                    Part::Text {
                                        text: format!("CSV Data:\n```\n{}\n```\n\nQuestion: ", text),
                                    },
                                );
                            }
                        }
                    }
                }
                Ok(BeforeModelResult::Continue(request))
            })
        }))
        .build()?;

    println!("Ask questions about the sales data:\n");

    adk_cli::console::run_console(Arc::new(agent), "csv_demo".to_string(), "user".to_string())
        .await?;

    Ok(())
}
