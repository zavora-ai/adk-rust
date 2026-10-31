//! Item 1: `RagTool` returns text, metadata, and score without embedding vectors, and its
//! `top_k` override still runs the reranker.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_agent::LlmAgentBuilder;
use adk_core::Tool;
use adk_model::GeminiModel;
use adk_rag::{
    Document, EmbeddingProvider, GeminiEmbeddingProvider, InMemoryVectorStore, RagConfig,
    RagPipeline, RagTool, RecursiveChunker, Reranker, Scope, SearchResult,
};
use adk_tool::SimpleToolContext;
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::common::{
    LIVE_MODEL, Mode, Report, Verdict, has_key, record_function_responses, run_turn, skip,
    verdict,
};

const POLICY: &str = "Returns are accepted within 30 days of delivery with the original \
                      receipt. Refunds go back to the original payment method.";
const SHIPPING: &str = "Standard shipping takes 3 to 5 business days. Express shipping \
                        arrives the next business day when ordered before noon.";
const WARRANTY: &str = "Electronics carry a two-year warranty that covers manufacturing \
                        defects but not accidental damage.";

/// A deterministic bag-of-words embedder: the user-supplied provider a test deployment uses.
struct WordHashEmbedder;

#[async_trait]
impl EmbeddingProvider for WordHashEmbedder {
    async fn embed(&self, text: &str) -> adk_rag::Result<Vec<f32>> {
        let mut vector = vec![0.0f32; 64];
        for word in text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
            let hash = word.to_lowercase().bytes().fold(7u32, |h, b| h.wrapping_mul(31) ^ b as u32);
            vector[hash as usize % 64] += 1.0;
        }
        Ok(vector)
    }

    fn dimensions(&self) -> usize {
        64
    }
}

/// Reverses the search order and counts its calls.
#[derive(Default)]
struct ReversingReranker {
    calls: AtomicUsize,
}

#[async_trait]
impl Reranker for ReversingReranker {
    async fn rerank(
        &self,
        _query: &str,
        mut results: Vec<SearchResult>,
    ) -> adk_rag::Result<Vec<SearchResult>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        results.reverse();
        Ok(results)
    }
}

fn documents() -> Vec<Document> {
    [("returns", POLICY), ("shipping", SHIPPING), ("warranty", WARRANTY)]
        .into_iter()
        .map(|(id, text)| Document::new(id, text).with_metadata("source", format!("{id}.md")))
        .collect()
}

async fn pipeline(
    embedder: Arc<dyn EmbeddingProvider>,
    reranker: Option<Arc<dyn Reranker>>,
) -> anyhow::Result<Arc<RagPipeline>> {
    let mut builder = RagPipeline::builder()
        .config(RagConfig::builder().chunk_size(300).chunk_overlap(50).top_k(3).build()?)
        .embedding_provider(embedder)
        .vector_store(Arc::new(InMemoryVectorStore::new()))
        .chunker(Arc::new(RecursiveChunker::new(300, 50)));
    if let Some(reranker) = reranker {
        builder = builder.reranker(reranker);
    }
    let pipeline = Arc::new(builder.build()?);
    pipeline.create_collection("kb").await?;
    pipeline.scoped(Scope::single_tenant()).ingest_batch("kb", &documents()).await?;
    Ok(pipeline)
}

async fn call(tool: &RagTool, args: Value) -> anyhow::Result<Value> {
    Ok(tool.execute(Arc::new(SimpleToolContext::new("release-tool-fixes")), args).await?)
}

/// The texts of the results, in order, whatever the result shape.
fn texts(result: &Value) -> Vec<String> {
    let items = result.get("results").unwrap_or(result);
    items
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            item.get("text").or_else(|| item.pointer("/chunk/text")).and_then(Value::as_str)
        })
        .map(str::to_owned)
        .collect()
}

pub async fn run(gemini_key: Option<&str>, report: &mut Report) {
    report.record_result("rag-result-has-no-vectors", 1, Mode::Local, result_shape().await);
    report.record_result("rag-top-k-is-reranked", 1, Mode::Local, top_k_reranked().await);
    match gemini_key {
        Some(key) => report.record_result("rag-live-model-input", 1, Mode::Live, live(key).await),
        None => report.record("rag-live-model-input", 1, Mode::Live, skip("no Gemini key")),
    }
}

/// The tool result carries text, metadata, and score, and no embedding.
async fn result_shape() -> anyhow::Result<Verdict> {
    let tool = RagTool::new(pipeline(Arc::new(WordHashEmbedder), None).await?, "kb");
    let result = call(&tool, json!({"query": "how do returns work"})).await?;
    let bytes = result.to_string().len();
    let found = texts(&result);
    Ok(verdict(vec![
        (!has_key(&result, "embedding"), "no `embedding` field in the result".to_string()),
        (!found.is_empty(), format!("{} results with text", found.len())),
        (result.to_string().contains("returns.md"), "metadata kept".to_string()),
        (result.to_string().contains("score"), "score kept".to_string()),
        (bytes < 2_000, format!("result is {bytes} bytes")),
    ]))
}

/// A `top_k` override returns that many results, reranked like the default path.
async fn top_k_reranked() -> anyhow::Result<Verdict> {
    let reranker = Arc::new(ReversingReranker::default());
    let tool = RagTool::new(
        pipeline(Arc::new(WordHashEmbedder), Some(reranker.clone() as Arc<dyn Reranker>)).await?,
        "kb",
    );
    call(&tool, json!({"query": "returns receipt"})).await?;
    let default_calls = reranker.calls.load(Ordering::SeqCst);
    let result = call(&tool, json!({"query": "returns receipt", "top_k": 2})).await?;
    let override_calls = reranker.calls.load(Ordering::SeqCst) - default_calls;
    let found = texts(&result);
    Ok(verdict(vec![
        (default_calls == 1, format!("default path reranked {default_calls} time(s)")),
        (override_calls == 1, format!("`top_k` override reranked {override_calls} time(s)")),
        (found.len() == 2, format!("`top_k: 2` returned {} results", found.len())),
    ]))
}

/// A live agent's model request carries no vectors in the `rag_search` response.
async fn live(key: &str) -> anyhow::Result<Verdict> {
    let embedder = Arc::new(GeminiEmbeddingProvider::new(key)?);
    let tool = RagTool::new(pipeline(embedder, None).await?, "kb");
    let (callback, seen) = record_function_responses();
    let agent = LlmAgentBuilder::new("support")
        .instruction(
            "Answer questions using the rag_search tool. Call it exactly once, then answer \
             in one sentence.",
        )
        .model(Arc::new(GeminiModel::new(key, LIVE_MODEL)?))
        .tool(Arc::new(tool))
        .before_model_callback(callback)
        .build()?;
    let turn = run_turn(Arc::new(agent), "How many days do I have to return an item?").await?;
    let responses: Vec<Value> = seen
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|(name, _)| name == "rag_search")
        .map(|(_, response)| response.clone())
        .collect();
    let bytes: usize = responses.iter().map(|response| response.to_string().len()).sum();
    let answer = turn.answer();
    Ok(verdict(vec![
        (turn.error.is_none(), format!("run error: {:?}", turn.error)),
        (!responses.is_empty(), format!("{} rag_search responses sent", responses.len())),
        (
            !responses.iter().any(|response| has_key(response, "embedding")),
            "no `embedding` in the model input".to_string(),
        ),
        (bytes < 4_000, format!("rag_search responses total {bytes} bytes")),
        (answer.contains("30"), format!("answer: {}", answer.trim())),
    ]))
}
