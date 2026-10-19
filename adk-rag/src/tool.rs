//! Agentic retrieval tool for ADK agents.
//!
//! The [`RagTool`] wraps a [`RagPipeline`] as an
//! [`adk_core::Tool`] so that agents can perform retrieval as a tool call.
//!
//! # Example
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use adk_rag::{RagPipeline, RagTool};
//!
//! let pipeline = Arc::new(build_pipeline()?);
//! let tool = RagTool::new(pipeline, "my_docs");
//!
//! // The agent calls the tool with:
//! // { "query": "How do I configure X?", "collection": "faq", "top_k": 5 }
//! // and receives, best first:
//! // [{ "id": "...", "document_id": "...", "text": "...", "metadata": {...}, "score": 0.83 }]
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use adk_core::{
    AdkError, ErrorCategory, ErrorComponent, TOOL_INVALID_ARGUMENTS_CODE, Tool, ToolContext,
};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};
use tracing::{error, info};

use crate::document::SearchResult;
use crate::pipeline::RagPipeline;

/// A retrieval tool that wraps a [`RagPipeline`] for agentic use.
///
/// Implements [`adk_core::Tool`] so it can be attached to any ADK agent.
/// The tool accepts a required `query` string and optional `collection`
/// and `top_k` parameters. Every query runs through the pipeline's reranker and
/// similarity threshold, including one that overrides `top_k`.
///
/// The result is an array of retrieved chunks, best first, each with its `id`,
/// `document_id`, `text`, `metadata`, and `score`. Embedding vectors stay out of the
/// result: they mean nothing to a model and would fill its context.
pub struct RagTool {
    pipeline: Arc<RagPipeline>,
    default_collection: String,
}

impl RagTool {
    /// Create a new `RagTool` backed by the given pipeline.
    ///
    /// The `default_collection` is used when the agent does not specify
    /// a collection in the tool call arguments.
    pub fn new(pipeline: Arc<RagPipeline>, default_collection: impl Into<String>) -> Self {
        Self { pipeline, default_collection: default_collection.into() }
    }
}

#[async_trait]
impl Tool for RagTool {
    fn name(&self) -> &str {
        "rag_search"
    }

    fn description(&self) -> &str {
        "Search a knowledge base for relevant documents given a query"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query to find relevant documents"
                },
                "collection": {
                    "type": "string",
                    "description": "The name of the collection to search. Uses the default collection if omitted."
                },
                "top_k": {
                    "type": "integer",
                    "description": "Maximum number of results to return. Uses the pipeline default if omitted."
                }
            },
            "required": ["query"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_arguments("missing required 'query' parameter"))?;

        let collection =
            args.get("collection").and_then(Value::as_str).unwrap_or(&self.default_collection);

        let top_k = match args.get("top_k") {
            None | Some(Value::Null) => self.pipeline.config().top_k,
            // Some providers send integers as integral floats.
            Some(value) => value
                .as_u64()
                .or_else(|| {
                    value.as_f64().filter(|f| f.fract() == 0.0 && *f >= 1.0).map(|f| f as u64)
                })
                .filter(|top_k| *top_k > 0)
                .and_then(|top_k| usize::try_from(top_k).ok())
                .ok_or_else(|| invalid_arguments("'top_k' must be a positive integer"))?,
        };

        info!(query, collection, top_k, "rag_search tool called");

        let results =
            self.pipeline.query_with_top_k(collection, query, top_k).await.map_err(|e| {
                error!(error = %e, "rag_search failed");
                AdkError::tool(format!("RAG search failed: {e}"))
            })?;

        let retrieved: Vec<RetrievedChunk<'_>> = results.iter().map(RetrievedChunk::from).collect();
        serde_json::to_value(&retrieved).map_err(|e| {
            error!(error = %e, "failed to serialize search results");
            AdkError::tool(format!("failed to serialize results: {e}"))
        })
    }
}

/// One search result as the model receives it: everything but the embedding.
#[derive(Serialize)]
struct RetrievedChunk<'a> {
    id: &'a str,
    document_id: &'a str,
    text: &'a str,
    metadata: &'a HashMap<String, String>,
    score: f32,
}

impl<'a> From<&'a SearchResult> for RetrievedChunk<'a> {
    fn from(result: &'a SearchResult) -> Self {
        Self {
            id: &result.chunk.id,
            document_id: &result.chunk.document_id,
            text: &result.chunk.text,
            metadata: &result.chunk.metadata,
            score: result.score,
        }
    }
}

/// An argument error the model sees in full, so it can correct the call.
fn invalid_arguments(message: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::InvalidInput,
        TOOL_INVALID_ARGUMENTS_CODE,
        format!("rag_search: {message}"),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    use super::*;
    use crate::chunking::FixedSizeChunker;
    use crate::config::RagConfig;
    use crate::document::Document;
    use crate::embedding::EmbeddingProvider;
    use crate::inmemory::InMemoryVectorStore;
    use crate::reranker::Reranker;

    /// Embeds each text as its length and vowel count, so results are deterministic.
    struct CountingEmbedder;

    #[async_trait]
    impl EmbeddingProvider for CountingEmbedder {
        async fn embed(&self, text: &str) -> crate::error::Result<Vec<f32>> {
            let vowels = text.chars().filter(|c| "aeiou".contains(*c)).count();
            Ok(vec![text.len() as f32, vowels as f32, 1.0])
        }

        fn dimensions(&self) -> usize {
            3
        }
    }

    /// Reverses the search order and counts how often it ran.
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
        ) -> crate::error::Result<Vec<SearchResult>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            results.reverse();
            Ok(results)
        }
    }

    async fn tool(reranker: Option<Arc<ReversingReranker>>) -> RagTool {
        let mut builder = RagPipeline::builder()
            .config(RagConfig::builder().top_k(3).build().expect("valid config"))
            .embedding_provider(Arc::new(CountingEmbedder))
            .vector_store(Arc::new(InMemoryVectorStore::new()))
            .chunker(Arc::new(FixedSizeChunker::try_new(64, 0).expect("valid sizes")));
        if let Some(reranker) = reranker {
            builder = builder.reranker(reranker);
        }
        let pipeline = Arc::new(builder.build().expect("build pipeline"));
        pipeline.create_collection("kb").await.expect("create collection");
        for (id, text) in
            [("a", "returns within thirty days"), ("b", "ships in two days"), ("c", "warranty")]
        {
            let document = Document {
                id: id.to_string(),
                text: text.to_string(),
                metadata: HashMap::from([("source".to_string(), format!("{id}.md"))]),
                source_uri: None,
            };
            pipeline.ingest("kb", &document).await.expect("ingest");
        }
        RagTool::new(pipeline, "kb")
    }

    fn context() -> Arc<dyn ToolContext> {
        Arc::new(adk_tool::SimpleToolContext::new("rag-tool-test"))
    }

    #[tokio::test]
    async fn results_carry_text_metadata_and_score_but_no_embedding() {
        let result =
            tool(None).await.execute(context(), json!({"query": "returns"})).await.expect("search");

        let results = result.as_array().expect("an array of results");
        assert_eq!(results.len(), 3);
        for item in results {
            // Key order depends on serde_json's `preserve_order` feature.
            let mut keys: Vec<&str> =
                item.as_object().expect("object").keys().map(String::as_str).collect();
            keys.sort_unstable();
            assert_eq!(keys, ["document_id", "id", "metadata", "score", "text"]);
        }
        let first = &results[0];
        assert_eq!(
            first["metadata"]["source"],
            format!("{}.md", first["document_id"].as_str().unwrap())
        );
        assert!(!result.to_string().contains("embedding"));
    }

    #[tokio::test]
    async fn a_top_k_override_is_reranked_like_the_default_query() {
        let reranker = Arc::new(ReversingReranker::default());
        let tool = tool(Some(reranker.clone())).await;
        let default = tool.execute(context(), json!({"query": "returns"})).await.expect("search");
        let overridden =
            tool.execute(context(), json!({"query": "returns", "top_k": 3})).await.expect("search");

        assert_eq!(reranker.calls.load(Ordering::SeqCst), 2, "both queries are reranked");
        assert_eq!(overridden, default);
        let limited = tool
            .execute(context(), json!({"query": "returns", "top_k": 2.0}))
            .await
            .expect("search");
        assert_eq!(limited.as_array().map(Vec::len), Some(2));
    }

    #[tokio::test]
    async fn a_top_k_that_is_not_a_positive_integer_is_an_argument_error() {
        let tool = tool(None).await;
        for top_k in [json!(0), json!(-1), json!(1.5), json!("3")] {
            let error = tool
                .execute(context(), json!({"query": "returns", "top_k": top_k}))
                .await
                .expect_err("invalid top_k");
            assert_eq!(error.code, TOOL_INVALID_ARGUMENTS_CODE, "top_k {top_k}");
        }
    }
}
