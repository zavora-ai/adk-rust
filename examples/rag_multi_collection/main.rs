//! # RAG Multi-Collection Support Agent
//!
//! A technical support agent that searches across multiple knowledge base
//! collections (docs, troubleshooting, changelog) using separate `RagTool`
//! instances. The agent decides which collection to search based on the
//! user's question.
//!
//! Requires: `GOOGLE_API_KEY` environment variable.
//!
//! Run: `cargo run --example rag_multi_collection --features rag-gemini`

use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_model::gemini::GeminiModel;
use adk_rag::{
    Document, GeminiEmbeddingProvider, InMemoryVectorStore, RagConfig, RagPipeline, RagTool,
    RecursiveChunker,
};

fn product_docs() -> Vec<Document> {
    vec![
        Document {
            id: "getting_started".into(),
            text: "To get started with CloudSync, create an account at app.cloudsync.io and \
                   install the CLI with `npm install -g cloudsync-cli`. Authenticate by running \
                   `cloudsync login` which opens a browser for OAuth. Once authenticated, \
                   initialize a project with `cloudsync init` in your repository root. This \
                   creates a .cloudsync.yaml config file."
                .into(),
            metadata: HashMap::from([("section".into(), "getting_started".into())]),
            source_uri: Some("docs/getting-started.md".into()),
        },
        Document {
            id: "sync_config".into(),
            text: "The .cloudsync.yaml file controls sync behavior. Key settings: \
                   `sync_interval` (default: 5m) sets how often files are synced. \
                   `ignore_patterns` accepts glob patterns for files to skip (e.g. node_modules, \
                   .git, *.log). `conflict_resolution` can be 'local-wins', 'remote-wins', or \
                   'manual' (default). `max_file_size` limits individual file sync to 100MB \
                   by default. Set `encryption: true` to enable AES-256 encryption at rest."
                .into(),
            metadata: HashMap::from([("section".into(), "configuration".into())]),
            source_uri: Some("docs/configuration.md".into()),
        },
        Document {
            id: "teams".into(),
            text: "CloudSync Teams allows shared workspaces for collaboration. Create a team \
                   with `cloudsync team create <name>`. Invite members via `cloudsync team invite \
                   <email>`. Team members can have roles: admin (full control), editor (sync and \
                   modify), or viewer (read-only). Team storage is pooled — the team plan \
                   determines total storage available across all members."
                .into(),
            metadata: HashMap::from([("section".into(), "teams".into())]),
            source_uri: Some("docs/teams.md".into()),
        },
    ]
}

fn troubleshooting_docs() -> Vec<Document> {
    vec![
        Document {
            id: "sync_stuck".into(),
            text: "Problem: Sync is stuck at 'Uploading...' and never completes.\n\
                   Cause: Usually a large file exceeding max_file_size or a network timeout.\n\
                   Fix: 1) Check `cloudsync status` for the stuck file. 2) Add the file to \
                   ignore_patterns if it shouldn't sync. 3) Increase max_file_size in config \
                   if the file is needed. 4) Run `cloudsync reset --soft` to clear the sync \
                   queue without losing data. 5) If the issue persists, run `cloudsync doctor` \
                   which diagnoses common problems."
                .into(),
            metadata: HashMap::from([("issue".into(), "sync_stuck".into())]),
            source_uri: Some("troubleshooting/sync-stuck.md".into()),
        },
        Document {
            id: "auth_expired".into(),
            text: "Problem: 'Authentication expired' error when running CLI commands.\n\
                   Cause: OAuth tokens expire after 24 hours of inactivity.\n\
                   Fix: Run `cloudsync login` to re-authenticate. If you're in a CI/CD \
                   environment, use `cloudsync login --token <api-token>` with a long-lived \
                   API token generated from the dashboard under Settings > API Tokens. \
                   API tokens don't expire but can be revoked from the dashboard."
                .into(),
            metadata: HashMap::from([("issue".into(), "auth_expired".into())]),
            source_uri: Some("troubleshooting/auth-expired.md".into()),
        },
        Document {
            id: "conflict_errors".into(),
            text: "Problem: 'Conflict detected' errors appearing frequently.\n\
                   Cause: Multiple users editing the same file simultaneously.\n\
                   Fix: 1) Set conflict_resolution to 'manual' to review each conflict. \
                   2) Use `cloudsync conflicts list` to see pending conflicts. 3) Resolve \
                   with `cloudsync conflicts resolve <file> --keep local|remote|both`. \
                   4) For teams, consider using file locking: `cloudsync lock <file>` \
                   prevents others from editing until you `cloudsync unlock <file>`."
                .into(),
            metadata: HashMap::from([("issue".into(), "conflicts".into())]),
            source_uri: Some("troubleshooting/conflicts.md".into()),
        },
    ]
}

fn changelog_docs() -> Vec<Document> {
    vec![
        Document {
            id: "v3_2_0".into(),
            text: "v3.2.0 (2025-11-15): Added file locking for team workspaces — use \
                   `cloudsync lock` and `cloudsync unlock` to prevent concurrent edits. \
                   New `cloudsync doctor` command diagnoses common sync issues automatically. \
                   Improved upload speed by 40% with parallel chunk uploads. Fixed a bug \
                   where ignore_patterns with double-star globs were not matching correctly."
                .into(),
            metadata: HashMap::from([("version".into(), "3.2.0".into())]),
            source_uri: Some("changelog/v3.2.0.md".into()),
        },
        Document {
            id: "v3_1_0".into(),
            text: "v3.1.0 (2025-08-20): Added AES-256 encryption at rest — enable with \
                   `encryption: true` in .cloudsync.yaml. New team roles: viewer role added \
                   for read-only access. `cloudsync status` now shows real-time progress \
                   with file-level detail. Fixed memory leak in the file watcher on Linux \
                   when monitoring directories with 10,000+ files."
                .into(),
            metadata: HashMap::from([("version".into(), "3.1.0".into())]),
            source_uri: Some("changelog/v3.1.0.md".into()),
        },
        Document {
            id: "v3_0_0".into(),
            text: "v3.0.0 (2025-05-01): Major release — complete rewrite of the sync engine. \
                   Breaking: config file renamed from .cloudsync to .cloudsync.yaml. New \
                   conflict resolution modes: local-wins, remote-wins, manual. Added team \
                   workspaces with shared storage pools. CLI now requires Node.js 18+. \
                   Dropped support for the legacy v1 API — migrate with `cloudsync migrate`."
                .into(),
            metadata: HashMap::from([("version".into(), "3.0.0".into())]),
            source_uri: Some("changelog/v3.0.0.md".into()),
        },
    ]
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key =
        std::env::var("GOOGLE_API_KEY").or_else(|_| std::env::var("GEMINI_API_KEY")).expect(
            "GOOGLE_API_KEY or GEMINI_API_KEY must be set.\n\
             Get a key at https://aistudio.google.com/apikey",
        );

    let config = RagConfig::builder()
        .chunk_size(350)
        .chunk_overlap(50)
        .top_k(3)
        .similarity_threshold(0.0)
        .build()?;

    let embedding = Arc::new(GeminiEmbeddingProvider::new(&api_key)?);
    let store = Arc::new(InMemoryVectorStore::new());
    let chunker = Arc::new(RecursiveChunker::new(350, 50));

    let pipeline = Arc::new(
        RagPipeline::builder()
            .config(config)
            .embedding_provider(embedding)
            .vector_store(store)
            .chunker(chunker)
            .build()?,
    );

    // Create three separate collections
    for name in &["docs", "troubleshooting", "changelog"] {
        pipeline.create_collection(name).await?;
    }

    // Ingest into each collection
    println!("Ingesting knowledge base...");
    for doc in &product_docs() {
        pipeline.ingest("docs", doc).await?;
    }
    for doc in &troubleshooting_docs() {
        pipeline.ingest("troubleshooting", doc).await?;
    }
    for doc in &changelog_docs() {
        pipeline.ingest("changelog", doc).await?;
    }
    println!("  docs: {} documents", product_docs().len());
    println!("  troubleshooting: {} documents", troubleshooting_docs().len());
    println!("  changelog: {} documents\n", changelog_docs().len());

    // Three RagTool instances — one per collection. The agent picks which to call.
    let docs_tool = RagTool::new(pipeline.clone(), "docs");
    let troubleshoot_tool = RagTool::new(pipeline.clone(), "troubleshooting");
    let changelog_tool = RagTool::new(pipeline, "changelog");

    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let agent = LlmAgentBuilder::new("cloudsync_support")
        .description("Technical support agent for CloudSync product.")
        .instruction(
            "You are a technical support agent for CloudSync, a file synchronization product. \
             You have access to three rag_search tools that search different knowledge bases:\n\
             - Use collection 'docs' for how-to questions, setup, and configuration\n\
             - Use collection 'troubleshooting' for error messages and problems\n\
             - Use collection 'changelog' for questions about versions, new features, or changes\n\n\
             Always search the relevant collection before answering. For ambiguous questions, \
             search multiple collections. Cite the source document when providing information. \
             If you can't find an answer, suggest the user open a support ticket at \
             support.cloudsync.io.",
        )
        .model(Arc::new(model))
        .tool(Arc::new(docs_tool))
        .tool(Arc::new(troubleshoot_tool))
        .tool(Arc::new(changelog_tool))
        .build()?;

    println!("CloudSync Support Agent ready. Ask about setup, troubleshooting, or features.\n");
    adk_cli::console::run_console(
        Arc::new(agent),
        "rag_multi_collection".into(),
        "customer1".into(),
    )
    .await?;

    Ok(())
}
