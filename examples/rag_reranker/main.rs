//! # RAG HR Policy Agent with Custom Reranker
//!
//! An HR assistant that answers employee questions about company policies.
//! Uses a custom `KeywordBoostReranker` to improve retrieval precision —
//! results containing query keywords get a score boost before being passed
//! to the LLM.
//!
//! Requires: `GOOGLE_API_KEY` environment variable.
//!
//! Run: `cargo run --example rag_reranker --features rag-gemini`

use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_model::gemini::GeminiModel;
use adk_rag::{
    Document, GeminiEmbeddingProvider, InMemoryVectorStore, MarkdownChunker, RagConfig,
    RagPipeline, RagTool, Reranker, SearchResult,
};

// ---------------------------------------------------------------------------
// KeywordBoostReranker — boosts results containing query keywords
// ---------------------------------------------------------------------------

struct KeywordBoostReranker {
    boost_per_keyword: f32,
}

impl KeywordBoostReranker {
    fn new(boost_per_keyword: f32) -> Self {
        Self { boost_per_keyword }
    }
}

#[async_trait::async_trait]
impl Reranker for KeywordBoostReranker {
    async fn rerank(
        &self,
        query: &str,
        mut results: Vec<SearchResult>,
    ) -> adk_rag::Result<Vec<SearchResult>> {
        let keywords: Vec<String> =
            query.split_whitespace().filter(|w| w.len() > 3).map(|w| w.to_lowercase()).collect();

        for result in &mut results {
            let text_lower = result.chunk.text.to_lowercase();
            let matches = keywords.iter().filter(|kw| text_lower.contains(kw.as_str())).count();
            result.score += matches as f32 * self.boost_per_keyword;
        }

        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        Ok(results)
    }
}

// ---------------------------------------------------------------------------
// Company policy documents
// ---------------------------------------------------------------------------

fn policy_documents() -> Vec<Document> {
    vec![
        Document {
            id: "pto_policy".into(),
            text: "# Paid Time Off (PTO) Policy\n\n\
                   ## Accrual\n\n\
                   Full-time employees accrue 20 days of PTO per year, prorated for the \
                   first year of employment. PTO accrues at 1.67 days per month. Part-time \
                   employees accrue PTO proportional to their scheduled hours.\n\n\
                   ## Requesting Time Off\n\n\
                   Submit PTO requests through the HR portal at least 5 business days in \
                   advance. Requests of 5+ consecutive days require manager approval and \
                   should be submitted 2 weeks ahead. Emergency time off can be requested \
                   same-day by notifying your manager directly.\n\n\
                   ## Carryover\n\n\
                   Up to 5 unused PTO days carry over to the next calendar year. Days \
                   beyond the carryover limit expire on December 31. Employees are \
                   encouraged to use their PTO throughout the year."
                .into(),
            metadata: HashMap::from([("policy".into(), "pto".into())]),
            source_uri: Some("policies/pto.md".into()),
        },
        Document {
            id: "remote_work".into(),
            text: "# Remote Work Policy\n\n\
                   ## Eligibility\n\n\
                   All employees who have completed their 90-day probation period are \
                   eligible for remote work. New hires must work on-site for the first \
                   90 days unless approved by their VP.\n\n\
                   ## Schedule\n\n\
                   The company operates on a hybrid model: employees are expected in the \
                   office Tuesday through Thursday. Monday and Friday are flexible remote \
                   days. Fully remote arrangements require director-level approval.\n\n\
                   ## Equipment\n\n\
                   The company provides a laptop, monitor, and $500 home office stipend \
                   for remote-eligible employees. Equipment must be returned upon separation. \
                   IT support is available via Slack #it-help or the IT portal."
                .into(),
            metadata: HashMap::from([("policy".into(), "remote_work".into())]),
            source_uri: Some("policies/remote_work.md".into()),
        },
        Document {
            id: "expense_policy".into(),
            text: "# Expense Reimbursement Policy\n\n\
                   ## Eligible Expenses\n\n\
                   Business travel, client meals, conference fees, and professional \
                   development courses are reimbursable. Personal expenses, commuting \
                   costs, and alcohol are not eligible.\n\n\
                   ## Submission Process\n\n\
                   Submit expense reports within 30 days of the expense via the Expensify \
                   app. Attach original receipts for all expenses over $25. Reports are \
                   reviewed by your manager and processed by Finance within 10 business days.\n\n\
                   ## Limits\n\n\
                   Meals: $75/person for client dinners, $25/person for team lunches. \
                   Hotels: up to $250/night in standard markets, $350/night in high-cost \
                   cities (NYC, SF, London). Flights: economy class for trips under 6 hours, \
                   business class for longer flights with VP approval."
                .into(),
            metadata: HashMap::from([("policy".into(), "expenses".into())]),
            source_uri: Some("policies/expenses.md".into()),
        },
        Document {
            id: "benefits".into(),
            text: "# Employee Benefits\n\n\
                   ## Health Insurance\n\n\
                   The company offers three health plans: Basic (100% company-paid), \
                   Plus ($50/month employee contribution), and Premium ($120/month). \
                   Dental and vision are included in Plus and Premium plans. Open \
                   enrollment is in November each year.\n\n\
                   ## 401(k)\n\n\
                   The company matches 401(k) contributions up to 4% of salary. Matching \
                   vests over 3 years: 33% after year 1, 66% after year 2, 100% after \
                   year 3. Employees can enroll or change contributions at any time.\n\n\
                   ## Professional Development\n\n\
                   Each employee receives a $2,000 annual learning budget for courses, \
                   books, and conferences. Unused budget does not carry over. Submit \
                   requests through the Learning Portal for pre-approval."
                .into(),
            metadata: HashMap::from([("policy".into(), "benefits".into())]),
            source_uri: Some("policies/benefits.md".into()),
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

    // MarkdownChunker splits by headers — each policy section becomes a chunk.
    let config = RagConfig::builder()
        .chunk_size(400)
        .chunk_overlap(50)
        .top_k(4)
        .similarity_threshold(0.0)
        .build()?;

    let pipeline = Arc::new(
        RagPipeline::builder()
            .config(config)
            .embedding_provider(Arc::new(GeminiEmbeddingProvider::new(&api_key)?))
            .vector_store(Arc::new(InMemoryVectorStore::new()))
            .chunker(Arc::new(MarkdownChunker::new(400, 50)))
            .reranker(Arc::new(KeywordBoostReranker::new(0.1)))
            .build()?,
    );

    let collection = "hr_policies";
    pipeline.create_collection(collection).await?;

    let documents = policy_documents();
    println!("Ingesting {} policy documents...", documents.len());
    for doc in &documents {
        let chunks = pipeline.ingest(collection, doc).await?;
        println!("  {} → {} chunks", doc.id, chunks.len());
    }

    let rag_tool = RagTool::new(pipeline, collection);
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let agent = LlmAgentBuilder::new("hr_assistant")
        .description("HR policy assistant that answers employee questions about company policies.")
        .instruction(
            "You are a friendly HR assistant. Use the rag_search tool to look up company \
             policies before answering any question. Always cite the specific policy document \
             and section. If a question falls outside documented policies, say so and suggest \
             the employee contact HR directly at hr@company.com. Be precise with numbers \
             (dollar amounts, day counts, percentages) — do not approximate.",
        )
        .model(Arc::new(model))
        .tool(Arc::new(rag_tool))
        .build()?;

    println!("\nHR Policy Agent ready. Ask about PTO, remote work, expenses, or benefits.\n");
    adk_cli::console::run_console(Arc::new(agent), "rag_reranker".into(), "employee1".into())
        .await?;

    Ok(())
}
