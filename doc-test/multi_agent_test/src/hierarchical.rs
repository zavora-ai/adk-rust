//! Hierarchical Multi-Agent System
//!
//! Demonstrates multi-level agent hierarchy:
//! Project Manager → Content Creator → (Researcher, Writer)
//!
//! Run with: cargo run --bin hierarchical

use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // Level 3: Leaf specialists
    let researcher = LlmAgentBuilder::new("researcher")
        .description("Researches topics and gathers comprehensive information")
        .instruction("You are a research specialist. When asked to research a topic:\n\
                     - Gather key facts and data\n\
                     - Identify main themes and subtopics\n\
                     - Note important sources or references\n\
                     Provide thorough, well-organized research summaries.")
        .model(model.clone())
        .build()?;

    let writer = LlmAgentBuilder::new("writer")
        .description("Writes polished content based on research")
        .instruction("You are a content writer. When asked to write:\n\
                     - Create engaging, clear content\n\
                     - Use appropriate tone for the audience\n\
                     - Structure content logically\n\
                     - Polish for grammar and style\n\
                     Produce professional, publication-ready content.")
        .model(model.clone())
        .build()?;

    // Level 2: Content coordinator
    let content_creator = LlmAgentBuilder::new("content_creator")
        .description("Coordinates content creation by delegating research and writing")
        .instruction("You are a content creation lead. For content requests:\n\n\
                     - If RESEARCH is needed: Transfer to researcher\n\
                     - If WRITING is needed: Transfer to writer\n\
                     - For PLANNING or overview: Handle yourself\n\n\
                     Coordinate between research and writing phases.")
        .model(model.clone())
        .sub_agent(Arc::new(researcher))
        .sub_agent(Arc::new(writer))
        .build()?;

    // Level 1: Top-level manager
    let project_manager = LlmAgentBuilder::new("project_manager")
        .description("Manages projects and coordinates with content team")
        .instruction("You are a project manager. For incoming requests:\n\n\
                     - For CONTENT creation tasks: Transfer to content_creator\n\
                     - For PROJECT STATUS or general questions: Handle yourself\n\n\
                     Keep track of overall project goals and deadlines.")
        .model(model.clone())
        .sub_agent(Arc::new(content_creator))
        .build()?;

    println!("📊 Hierarchical Multi-Agent System");
    println!();
    println!("   project_manager");
    println!("       └── content_creator");
    println!("               ├── researcher");
    println!("               └── writer");
    println!();
    println!("Example prompts:");
    println!("   • 'Create a blog post about AI in healthcare'");
    println!("   • 'Research the history of electric vehicles'");
    println!("   • 'Write a product description for a smart watch'");
    println!();

    Launcher::new(Arc::new(project_manager)).run().await?;
    Ok(())
}
