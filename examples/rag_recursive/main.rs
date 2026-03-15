//! # RAG Codebase Q&A Agent
//!
//! A developer assistant that answers questions about a codebase by ingesting
//! source file documentation. Uses `RecursiveChunker` for natural paragraph
//! and sentence boundaries — ideal for code comments and technical docs.
//!
//! Requires: `GOOGLE_API_KEY` environment variable.
//!
//! Run: `cargo run --example rag_recursive --features rag-gemini`

use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_model::gemini::GeminiModel;
use adk_rag::{
    Document, GeminiEmbeddingProvider, InMemoryVectorStore, RagConfig, RagPipeline, RagTool,
    RecursiveChunker,
};

fn codebase_docs() -> Vec<Document> {
    vec![
        Document {
            id: "architecture".into(),
            text: "The application follows a layered architecture with three main layers: \
                   the API layer (axum handlers), the service layer (business logic), and \
                   the repository layer (database access via sqlx).\n\n\
                   The API layer validates incoming requests using serde and returns JSON \
                   responses. Each handler calls into the service layer which orchestrates \
                   business rules. The repository layer uses prepared statements and \
                   connection pooling via PgPool.\n\n\
                   Cross-cutting concerns like authentication, logging, and error handling \
                   are implemented as axum middleware. The auth middleware extracts JWT \
                   tokens from the Authorization header and validates them against the \
                   signing key stored in environment variables."
                .into(),
            metadata: HashMap::from([
                ("file".into(), "docs/architecture.md".into()),
                ("area".into(), "architecture".into()),
            ]),
            source_uri: Some("docs/architecture.md".into()),
        },
        Document {
            id: "database".into(),
            text: "Database migrations are managed with sqlx-cli. Run `sqlx migrate run` \
                   to apply pending migrations. The migrations directory is at `migrations/`.\n\n\
                   The schema includes four main tables: users, projects, tasks, and comments. \
                   Users have a one-to-many relationship with projects. Projects contain tasks \
                   which can have nested comments. All tables use UUID primary keys and include \
                   created_at and updated_at timestamps.\n\n\
                   Connection pooling is configured in config.rs with a default pool size of 10. \
                   Set DATABASE_URL in .env to point to your PostgreSQL instance. For testing, \
                   use DATABASE_URL_TEST which creates an isolated test database."
                .into(),
            metadata: HashMap::from([
                ("file".into(), "docs/database.md".into()),
                ("area".into(), "database".into()),
            ]),
            source_uri: Some("docs/database.md".into()),
        },
        Document {
            id: "api_endpoints".into(),
            text: "POST /api/auth/register — Create a new user account. Requires email, \
                   password (min 8 chars), and display_name. Returns the user object with \
                   an access token.\n\n\
                   POST /api/auth/login — Authenticate with email and password. Returns \
                   access_token (1h expiry) and refresh_token (30d expiry).\n\n\
                   GET /api/projects — List all projects for the authenticated user. Supports \
                   ?page and ?limit query parameters. Returns paginated results with total count.\n\n\
                   POST /api/projects/:id/tasks — Create a task within a project. Requires \
                   title and optional description, priority (low/medium/high), and due_date. \
                   The assignee defaults to the creating user."
                .into(),
            metadata: HashMap::from([
                ("file".into(), "docs/api.md".into()),
                ("area".into(), "api".into()),
            ]),
            source_uri: Some("docs/api.md".into()),
        },
        Document {
            id: "deployment".into(),
            text: "The application is deployed as a Docker container on AWS ECS Fargate. \
                   The Dockerfile uses a multi-stage build: the first stage compiles the \
                   Rust binary with cargo build --release, and the second stage copies \
                   the binary into a minimal debian-slim image.\n\n\
                   CI/CD is handled by GitHub Actions. On push to main, the pipeline runs \
                   tests, builds the Docker image, pushes to ECR, and updates the ECS \
                   service. Environment variables are injected via AWS Secrets Manager.\n\n\
                   Health checks are exposed at GET /health which returns 200 OK when the \
                   server is ready. The ECS task definition configures a health check \
                   interval of 30 seconds with a 5-second timeout."
                .into(),
            metadata: HashMap::from([
                ("file".into(), "docs/deployment.md".into()),
                ("area".into(), "deployment".into()),
            ]),
            source_uri: Some("docs/deployment.md".into()),
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

    // RecursiveChunker splits by paragraphs first, then sentences — good for
    // technical documentation where paragraph boundaries are meaningful.
    let config = RagConfig::builder()
        .chunk_size(300)
        .chunk_overlap(50)
        .top_k(3)
        .similarity_threshold(0.0)
        .build()?;

    let pipeline = Arc::new(
        RagPipeline::builder()
            .config(config)
            .embedding_provider(Arc::new(GeminiEmbeddingProvider::new(&api_key)?))
            .vector_store(Arc::new(InMemoryVectorStore::new()))
            .chunker(Arc::new(RecursiveChunker::new(300, 50)))
            .build()?,
    );

    let collection = "codebase";
    pipeline.create_collection(collection).await?;

    let documents = codebase_docs();
    println!("Ingesting {} codebase docs...", documents.len());
    let all_chunks = pipeline.ingest_batch(collection, &documents).await?;
    println!("Created {} chunks total.\n", all_chunks.len());

    let rag_tool = RagTool::new(pipeline, collection);
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let agent = LlmAgentBuilder::new("codebase_qa")
        .description("Developer assistant that answers questions about the codebase.")
        .instruction(
            "You are a senior developer assistant for a Rust web application. Use the \
             rag_search tool to look up information from the codebase documentation before \
             answering. When citing information, mention the source file (available in the \
             chunk metadata). If the docs don't cover the question, say so and suggest \
             where the developer might look.",
        )
        .model(Arc::new(model))
        .tool(Arc::new(rag_tool))
        .build()?;

    println!("Codebase Q&A agent ready. Ask about architecture, database, API, or deployment.\n");
    adk_cli::console::run_console(Arc::new(agent), "rag_recursive".into(), "dev1".into()).await?;

    Ok(())
}
