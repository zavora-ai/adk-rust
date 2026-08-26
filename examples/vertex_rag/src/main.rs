//! # Vertex AI RAG Engine — agentic corpus retrieval
//!
//! Verifies a pre-provisioned RAG corpus is ready, then runs an `LlmAgent`
//! that grounds its answers with `VertexAiRagRetrievalTool`: the model calls
//! `vertex_rag_retrieval` with a query and receives the most relevant
//! passages from the corpus.
//!
//! ```bash
//! gcloud auth application-default login
//! cargo run --manifest-path examples/vertex_rag/Cargo.toml
//! ```
//!
//! Requires `GOOGLE_API_KEY` (Gemini LLM provider), `GOOGLE_CLOUD_PROJECT`,
//! `GOOGLE_CLOUD_LOCATION`, and `VERTEX_RAG_CORPUS` naming a corpus with
//! imported files.

use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{Content, Part, SessionId, UserId};
use adk_model::GeminiModel;
use adk_rag::vertex_rag::{VertexAiRagRetrievalTool, VertexRagConfig, VertexRagEngineClient};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use tracing::info;

const APP_NAME: &str = "vertex-rag-example";
const SESSION_ID: &str = "vertex-rag-session";

fn require_env(name: &str) -> anyhow::Result<String> {
    std::env::var(name).map_err(|_| {
        anyhow::anyhow!(
            "Missing required environment variable: {name}\n\
             Set it in your .env file or export it in your shell.\n\
             See .env.example for all required variables."
        )
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    println!();
    println!("╔════════════════════════════════════════════════════════════╗");
    println!("║   Vertex AI RAG Engine — agentic corpus retrieval          ║");
    println!("║                                                            ║");
    println!("║   Env: GOOGLE_API_KEY, GOOGLE_CLOUD_PROJECT,               ║");
    println!("║        GOOGLE_CLOUD_LOCATION, VERTEX_RAG_CORPUS            ║");
    println!("╚════════════════════════════════════════════════════════════╝");
    println!();

    let api_key = require_env("GOOGLE_API_KEY")?;
    let corpus = require_env("VERTEX_RAG_CORPUS")?;

    // 1. Connect to the RAG Engine and verify the corpus is ready: the check
    //    fails with actionable guidance when the corpus is missing or empty.
    let config = VertexRagConfig::from_env()?;
    let client = Arc::new(VertexRagEngineClient::new_with_adc(config)?);
    let ready = client.ensure_corpus_ready(&corpus).await?;
    info!(
        rag.corpus = ready.name.as_deref(),
        rag.display_name = ready.display_name.as_deref(),
        rag.files = ready.rag_files_count,
        "corpus is ready for retrieval"
    );

    // 2. Expose retrieval as a tool the agent can call.
    let retrieval_tool = VertexAiRagRetrievalTool::new(client, vec![corpus]).similarity_top_k(5);

    // 3. Build the agent: it decides when to call vertex_rag_retrieval.
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-3.7-flash")?);
    let agent = Arc::new(
        LlmAgentBuilder::new("rag-assistant")
            .description("Answers questions grounded in a Vertex AI RAG Engine corpus")
            .model(model)
            .tool(Arc::new(retrieval_tool))
            .instruction(
                "Answer the user's question using the vertex_rag_retrieval tool. \
                 Always retrieve first, then answer strictly from the retrieved passages, \
                 citing sourceDisplayName for each fact. If nothing relevant is retrieved, \
                 say so instead of guessing.",
            )
            .build()?,
    );

    // 4. Run one grounded question through the runner.
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP_NAME.into(),
            user_id: "user".into(),
            session_id: Some(SESSION_ID.into()),
            state: HashMap::new(),
        })
        .await?;
    let runner =
        Runner::builder().app_name(APP_NAME).agent(agent).session_service(sessions).build()?;

    let question = std::env::var("RAG_QUESTION")
        .unwrap_or_else(|_| "What topics does this corpus cover? Summarize briefly.".to_string());
    println!("Question: {question}\n");

    let mut stream = runner
        .run(
            UserId::new("user")?,
            SessionId::new(SESSION_ID)?,
            Content::new("user").with_text(&question),
        )
        .await?;

    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::Text { text } if !text.trim().is_empty() => {
                        println!("💬 Agent: {text}");
                    }
                    Part::FunctionCall { name, args, .. } => {
                        println!("🔧 Tool call: {name}({args})");
                    }
                    Part::FunctionResponse { function_response, .. } => {
                        let passages = function_response
                            .response
                            .as_array()
                            .map(std::vec::Vec::len)
                            .unwrap_or_default();
                        println!("📚 {} returned {passages} passage(s)", function_response.name);
                    }
                    _ => {}
                }
            }
        }
    }

    Ok(())
}
