//! # Example Store — dynamic few-shot retrieval
//!
//! Upserts a handful of few-shot examples into a pre-provisioned Vertex AI
//! Example Store, then retrieves the most similar ones for a query. The same
//! retrieval powers `ExampleStoreProvider`, which injects results into agent
//! requests as a `BeforeModelCallback`.
//!
//! ```bash
//! gcloud auth application-default login
//! cargo run --manifest-path examples/example_store/Cargo.toml
//! ```
//!
//! Requires `GOOGLE_CLOUD_PROJECT`, `GOOGLE_CLOUD_LOCATION` (Example Store is
//! v1beta1 Preview, `us-central1` only), and `EXAMPLE_STORE_ID` naming a
//! pre-provisioned store.

use adk_core::Content;
use adk_tool::example_store::{
    ContentsExample, Example, ExampleStoreClient, ExampleStoreConfig, ExampleStoreProvider,
    SearchExamplesRequest, StoredContentsExample, UpsertExamplesRequest,
};
use std::sync::Arc;
use tracing::info;

fn support_example(id: &str, question: &str, answer: &str) -> Example {
    Example::new(
        StoredContentsExample::new(ContentsExample::new(
            vec![Content::new("user").with_text(question)],
            vec![Content::new("model").with_text(answer)],
        ))
        .with_search_key(question),
    )
    .with_example_id(id)
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
    println!("║   Example Store — dynamic few-shot retrieval               ║");
    println!("║                                                            ║");
    println!("║   Env: GOOGLE_CLOUD_PROJECT, GOOGLE_CLOUD_LOCATION,        ║");
    println!("║        EXAMPLE_STORE_ID (pre-provisioned, us-central1)     ║");
    println!("╚════════════════════════════════════════════════════════════╝");
    println!();

    let config = ExampleStoreConfig::from_env()?;
    let client = Arc::new(ExampleStoreClient::new_with_adc(config)?);
    info!(store = client.store_resource_name(), "connected to example store");

    // 1. Upsert a few support-style examples (overwrite keeps reruns idempotent).
    let upsert = client
        .upsert_examples(
            UpsertExamplesRequest::new(vec![
                support_example(
                    "adk-demo-password-reset",
                    "How do I reset my password?",
                    "Open Settings → Security → Reset password, then follow the email link.",
                ),
                support_example(
                    "adk-demo-cancel-plan",
                    "How do I cancel my subscription?",
                    "Go to Billing → Manage plan → Cancel. Your plan stays active until the period ends.",
                ),
                support_example(
                    "adk-demo-invoice",
                    "Where can I download my invoices?",
                    "Invoices are under Billing → History; each row has a PDF download.",
                ),
            ])
            .with_overwrite(true),
        )
        .await?;
    for result in &upsert.results {
        match (&result.example, &result.status) {
            (Some(example), _) => {
                info!(example.id = example.example_id.as_deref(), "upserted example");
            }
            (None, Some(status)) => {
                tracing::warn!(status.code = status.code, status.message = %status.message, "example rejected");
            }
            (None, None) => tracing::warn!("upsert result carried neither example nor status"),
        }
    }

    // 2. Search for the examples most similar to an incoming user question.
    let query = "I forgot my login credentials";
    println!("Query: {query}\n");
    let search = client.search_examples(SearchExamplesRequest::by_search_key(query, 3)).await?;
    for (index, result) in search.results.iter().enumerate() {
        let key = result.example.stored_contents_example.search_key.as_deref().unwrap_or("<none>");
        println!(
            "  {}. score {:.3}  search_key: {key}",
            index + 1,
            result.similarity_score.unwrap_or_default(),
        );
    }
    println!();

    // 3. The same retrieval, formatted the way ExampleStoreProvider injects it
    //    into agent requests via a BeforeModelCallback.
    let provider = ExampleStoreProvider::new(client).with_top_k(3);
    let results = provider.retrieve(query).await?;
    println!("{}", ExampleStoreProvider::format_examples(&results));

    Ok(())
}
