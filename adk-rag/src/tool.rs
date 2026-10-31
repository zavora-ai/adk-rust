//! Agentic retrieval tool for ADK agents.
//!
//! The [`RagTool`] wraps a [`RagPipeline`] as an
//! [`adk_core::Tool`] so that agents can perform retrieval as a tool call.
//!
//! The tool searches only the collections it is configured with. A one-collection
//! tool ([`RagTool::new`]) offers the model no `collection` argument; a tool given more
//! collections with [`RagTool::with_allowed_collections`] lists them in its schema and
//! refuses any other name.
//!
//! # Example
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use adk_rag::{RagPipeline, RagTool};
//!
//! let pipeline = Arc::new(build_pipeline()?);
//! let tool = RagTool::new(pipeline, "docs").with_allowed_collections(["faq"]);
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
use tracing::{error, info, warn};

use crate::document::SearchResult;
use crate::pipeline::RagPipeline;

/// A retrieval tool that wraps a [`RagPipeline`] for agentic use.
///
/// Implements [`adk_core::Tool`] so it can be attached to any ADK agent.
/// The tool accepts a required `query` string and an optional `top_k`. Every query
/// runs through the pipeline's reranker and similarity threshold, including one that
/// overrides `top_k`.
///
/// The collections are configuration, not model input. [`RagTool::new`] binds the
/// tool to one collection and its schema has no `collection` argument.
/// [`RagTool::with_allowed_collections`] lets the model choose among further
/// collections, which the schema lists; a call that names any other collection fails
/// with `tool.invalid_arguments` before the vector store is queried. One collection per
/// tenant therefore needs one tool per tenant, built with that tenant's collection.
///
/// The result is an array of retrieved chunks, best first, each with its `id`,
/// `document_id`, `text`, `metadata`, and `score`. Embedding vectors stay out of the
/// result: they mean nothing to a model and would fill its context.
///
/// The tool is read-only and concurrency-safe, so `DeclarativePolicy::allow_read_only`
/// admits it and `ToolExecutionStrategy::Auto` overlaps it with adjacent read-only calls.
pub struct RagTool {
    pipeline: Arc<RagPipeline>,
    default_collection: String,
    /// Collections a call may name besides the default, in schema order.
    other_collections: Vec<String>,
}

impl RagTool {
    /// Create a new `RagTool` that searches `default_collection` only.
    ///
    /// The tool's schema has no `collection` argument. A call that still names a
    /// collection other than `default_collection` fails with `tool.invalid_arguments`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use std::sync::Arc;
    /// use adk_rag::RagTool;
    ///
    /// // One tool per tenant, each bound to that tenant's collection.
    /// let tool = RagTool::new(Arc::clone(&pipeline), format!("kb_{tenant_id}"));
    /// ```
    pub fn new(pipeline: Arc<RagPipeline>, default_collection: impl Into<String>) -> Self {
        Self { pipeline, default_collection: default_collection.into(), other_collections: vec![] }
    }

    /// Replace the collections the model may name besides the default.
    ///
    /// The schema's `collection` argument lists the default first, then these. A call
    /// without `collection` searches the default; a call naming a collection outside
    /// the list fails with `tool.invalid_arguments`, whose message names the allowed
    /// collections. An empty list returns the tool to its one-collection form.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use std::sync::Arc;
    /// use adk_rag::RagTool;
    ///
    /// // Searches "docs" by default; the model may also choose "faq" or "changelog".
    /// let tool = RagTool::new(pipeline, "docs").with_allowed_collections(["faq", "changelog"]);
    /// ```
    #[must_use]
    pub fn with_allowed_collections<I, S>(mut self, collections: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.other_collections.clear();
        for collection in collections.into_iter().map(Into::into) {
            if collection != self.default_collection
                && !self.other_collections.contains(&collection)
            {
                self.other_collections.push(collection);
            }
        }
        self
    }

    /// The default collection, then the others a call may name.
    fn allowed_collections(&self) -> impl Iterator<Item = &str> {
        std::iter::once(&self.default_collection).chain(&self.other_collections).map(String::as_str)
    }

    /// Tells the model which collections it may name.
    fn collection_guidance(&self) -> String {
        let default = &self.default_collection;
        if self.other_collections.is_empty() {
            return format!("it searches only '{default}', so omit 'collection'");
        }
        let allowed = self
            .allowed_collections()
            .map(|name| format!("'{name}'"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("choose one of {allowed}, or omit 'collection' to search '{default}'")
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
        let mut properties = json!({
            "query": {
                "type": "string",
                "description": "The search query to find relevant documents"
            },
            "top_k": {
                "type": "integer",
                "description": "Maximum number of results to return. Uses the pipeline default if omitted."
            }
        });
        if !self.other_collections.is_empty() {
            let allowed: Vec<&str> = self.allowed_collections().collect();
            properties["collection"] = json!({
                "type": "string",
                "enum": allowed,
                "description": format!(
                    "The collection to search. Searches '{}' if omitted.",
                    self.default_collection
                )
            });
        }
        Some(json!({
            "type": "object",
            "properties": properties,
            "required": ["query"]
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_arguments("missing required 'query' parameter"))?;

        let collection = match args.get("collection") {
            None | Some(Value::Null) => self.default_collection.as_str(),
            Some(Value::String(requested))
                if self.allowed_collections().any(|allowed| allowed == requested) =>
            {
                requested.as_str()
            }
            Some(Value::String(requested)) => {
                warn!(
                    collection = requested.as_str(),
                    default_collection = %self.default_collection,
                    "rag_search refused a collection outside its allowlist"
                );
                return Err(invalid_arguments(&format!(
                    "collection '{requested}' is not searchable by this tool; {}",
                    self.collection_guidance()
                )));
            }
            Some(_) => {
                return Err(invalid_arguments(&format!(
                    "'collection' must be a string; {}",
                    self.collection_guidance()
                )));
            }
        };

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

    /// A pipeline with three collections: "kb" holds documents a, b, and c, "faq" holds
    /// "faq", and "tenant_b" holds "secret".
    async fn pipeline(reranker: Option<Arc<ReversingReranker>>) -> Arc<RagPipeline> {
        let mut builder = RagPipeline::builder()
            .config(RagConfig::builder().top_k(3).build().expect("valid config"))
            .embedding_provider(Arc::new(CountingEmbedder))
            .vector_store(Arc::new(InMemoryVectorStore::new()))
            .chunker(Arc::new(FixedSizeChunker::try_new(64, 0).expect("valid sizes")));
        if let Some(reranker) = reranker {
            builder = builder.reranker(reranker);
        }
        let pipeline = Arc::new(builder.build().expect("build pipeline"));
        for (collection, id, text) in [
            ("kb", "a", "returns within thirty days"),
            ("kb", "b", "ships in two days"),
            ("kb", "c", "warranty"),
            ("faq", "faq", "returns are free"),
            ("tenant_b", "secret", "returns for tenant b only"),
        ] {
            pipeline.create_collection(collection).await.expect("create collection");
            let document = Document {
                id: id.to_string(),
                text: text.to_string(),
                metadata: HashMap::from([("source".to_string(), format!("{id}.md"))]),
                source_uri: None,
            };
            pipeline.ingest(collection, &document).await.expect("ingest");
        }
        pipeline
    }

    async fn tool(reranker: Option<Arc<ReversingReranker>>) -> RagTool {
        RagTool::new(pipeline(reranker).await, "kb")
    }

    /// The document ids a search returned, sorted.
    fn document_ids(result: &Value) -> Vec<&str> {
        let mut ids: Vec<&str> = result
            .as_array()
            .expect("an array of results")
            .iter()
            .map(|item| item["document_id"].as_str().expect("document_id"))
            .collect();
        ids.sort_unstable();
        ids
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

    #[tokio::test]
    async fn a_one_collection_tool_offers_no_collection_and_refuses_another() {
        let tool = tool(None).await;
        assert_eq!(
            tool.parameters_schema(),
            Some(json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query to find relevant documents"
                    },
                    "top_k": {
                        "type": "integer",
                        "description": "Maximum number of results to return. Uses the pipeline default if omitted."
                    }
                },
                "required": ["query"]
            }))
        );

        let named = tool
            .execute(context(), json!({"query": "returns", "collection": "kb"}))
            .await
            .expect("its own collection");
        let omitted = tool.execute(context(), json!({"query": "returns"})).await.expect("search");
        assert_eq!(named, omitted);

        // "tenant_b" exists in the store, so only the allowlist keeps it out.
        let error = tool
            .execute(context(), json!({"query": "returns", "collection": "tenant_b"}))
            .await
            .expect_err("another collection");
        assert_eq!(
            (error.category, error.code, error.message.as_str()),
            (
                ErrorCategory::InvalidInput,
                TOOL_INVALID_ARGUMENTS_CODE,
                "rag_search: collection 'tenant_b' is not searchable by this tool; it searches \
                 only 'kb', so omit 'collection'"
            )
        );
    }

    #[tokio::test]
    async fn an_allowed_collection_is_searched_and_any_other_is_refused() {
        let tool =
            RagTool::new(pipeline(None).await, "kb").with_allowed_collections(["faq", "kb", "faq"]);
        assert_eq!(
            tool.parameters_schema().map(|schema| schema["properties"]["collection"].clone()),
            Some(json!({
                "type": "string",
                "enum": ["kb", "faq"],
                "description": "The collection to search. Searches 'kb' if omitted."
            }))
        );

        let faq = tool
            .execute(context(), json!({"query": "returns", "collection": "faq"}))
            .await
            .expect("an allowed collection");
        assert_eq!(document_ids(&faq), ["faq"]);

        let allowed = "choose one of 'kb', 'faq', or omit 'collection' to search 'kb'";
        for (collection, message) in [
            (
                json!("tenant_b"),
                format!(
                    "rag_search: collection 'tenant_b' is not searchable by this tool; {allowed}"
                ),
            ),
            (json!(7), format!("rag_search: 'collection' must be a string; {allowed}")),
        ] {
            let error = tool
                .execute(context(), json!({"query": "returns", "collection": collection}))
                .await
                .expect_err("refused collection");
            assert_eq!(
                (error.category, error.code, error.message),
                (ErrorCategory::InvalidInput, TOOL_INVALID_ARGUMENTS_CODE, message)
            );
        }
    }

    #[tokio::test]
    async fn a_call_without_a_collection_searches_the_default() {
        let tool =
            RagTool::new(pipeline(None).await, "kb").with_allowed_collections(["faq", "tenant_b"]);
        for args in [json!({"query": "returns"}), json!({"query": "returns", "collection": null})] {
            let result = tool.execute(context(), args).await.expect("search");
            assert_eq!(document_ids(&result), ["a", "b", "c"]);
        }
    }

    #[tokio::test]
    async fn the_tool_is_read_only_and_concurrency_safe() {
        let tool = tool(None).await;
        assert_eq!(
            (tool.is_read_only(), tool.is_concurrency_safe(), tool.effect()),
            (true, true, adk_core::ToolEffect::ReadOnly)
        );
    }
}
