//! # RAG Agent Example
//!
//! Demonstrates an `LlmAgent` that uses `RagTool` to answer questions from
//! an ingested knowledge base. The agent decides when to call `rag_search`
//! and uses the retrieved context to generate answers.
//!
//! Uses `GeminiEmbeddingProvider` for real embeddings, `InMemoryVectorStore`,
//! and `RecursiveChunker`.
//!
//! Requires: `GOOGLE_API_KEY` environment variable.
//!
//! Run: `cargo run --example rag_agent --features rag-gemini`

use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_model::gemini::GeminiModel;
use adk_rag::{
    Document, GeminiEmbeddingProvider, InMemoryVectorStore, RagConfig, RagPipeline, RagTool,
    RecursiveChunker,
};

// ---------------------------------------------------------------------------
// Sample knowledge base about a fictional product
// ---------------------------------------------------------------------------

fn sample_documents() -> Vec<Document> {
    vec![
        Document {
            id: "features".into(),
            text: "AcmeBot is an AI-powered customer support assistant. It can handle \
                   ticket routing, FAQ answering, and sentiment analysis. AcmeBot supports \
                   multi-language conversations in English, Spanish, French, and Japanese. \
                   It integrates with Slack, Microsoft Teams, and email via SMTP."
                .into(),
            metadata: HashMap::from([("category".into(), "features".into())]),
            source_uri: None,
        },
        Document {
            id: "pricing".into(),
            text: "AcmeBot pricing starts at $29/month for the Starter plan which includes \
                   up to 1,000 conversations per month. The Professional plan at $99/month \
                   supports 10,000 conversations and adds priority support. The Enterprise \
                   plan offers unlimited conversations, custom integrations, and a dedicated \
                   account manager — contact sales for pricing."
                .into(),
            metadata: HashMap::from([("category".into(), "pricing".into())]),
            source_uri: None,
        },
        Document {
            id: "faq".into(),
            text: "Frequently Asked Questions about AcmeBot: \
                   Q: How do I reset my API key? A: Go to Settings > API Keys > Regenerate. \
                   Q: Does AcmeBot store conversation data? A: Yes, conversations are stored \
                   for 90 days by default. You can configure retention in the admin panel. \
                   Q: Can I train AcmeBot on my own data? A: Yes, upload documents through \
                   the Knowledge Base section in the dashboard."
                .into(),
            metadata: HashMap::from([("category".into(), "faq".into())]),
            source_uri: None,
        },
    ]
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env if present (for GOOGLE_API_KEY).
    dotenvy::dotenv().ok();

    // -- 1. Load API key --------------------------------------------------
    let api_key =
        std::env::var("GOOGLE_API_KEY").or_else(|_| std::env::var("GEMINI_API_KEY")).expect(
            "GOOGLE_API_KEY or GEMINI_API_KEY must be set.\n\
             Get a key at https://aistudio.google.com/apikey",
        );

    // -- 2. Configure the RAG pipeline ------------------------------------
    // RecursiveChunker splits by paragraphs first, then sentences, then words.
    // chunk_size=300 and overlap=50 work well for short product docs.
    let config = RagConfig::builder()
        .chunk_size(300)
        .chunk_overlap(50)
        .top_k(3)
        .similarity_threshold(0.0)
        .build()?;

    // GeminiEmbeddingProvider uses the gemini-embedding-001 model.
    let embedding_provider = Arc::new(GeminiEmbeddingProvider::new(&api_key)?);

    let pipeline = Arc::new(
        RagPipeline::builder()
            .config(config)
            .embedding_provider(embedding_provider)
            .vector_store(Arc::new(InMemoryVectorStore::new()))
            .chunker(Arc::new(RecursiveChunker::new(300, 50)))
            .build()?,
    );

    // -- 3. Ingest the knowledge base -------------------------------------
    let collection = "acmebot_docs";
    pipeline.create_collection(collection).await?;

    let documents = sample_documents();
    println!("Ingesting {} documents into knowledge base...", documents.len());
    for doc in &documents {
        let chunks = pipeline.ingest(collection, doc).await?;
        println!("  {} → {} chunk(s)", doc.id, chunks.len());
    }

    // -- 4. Create the RagTool and attach it to an LlmAgent ---------------
    // RagTool wraps the pipeline as an adk_core::Tool so the agent can call
    // "rag_search" whenever it needs context from the knowledge base.
    let rag_tool = RagTool::new(pipeline, collection);

    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let agent = LlmAgentBuilder::new("acmebot_support")
        .description("Customer support agent for AcmeBot product questions.")
        .instruction(
            "You are a helpful support agent for AcmeBot. Use the rag_search tool to look up \
             information from the knowledge base before answering. Always cite the source \
             category when providing information. If the knowledge base does not contain \
             relevant information, say so honestly.",
        )
        .model(Arc::new(model))
        .tool(Arc::new(rag_tool))
        .build()?;

    // -- 5. Run the agent in console mode ---------------------------------
    println!("\nAgent ready. Ask questions about AcmeBot (e.g. pricing, features, FAQ).\n");

    let app_name = "rag_agent".to_string();
    let user_id = "user1".to_string();
    adk_cli::console::run_console(Arc::new(agent), app_name, user_id).await?;

    Ok(())
}
