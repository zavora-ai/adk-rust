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
//! ```

use std::sync::Arc;

use adk_core::{AdkError, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};
use tracing::{error, info};

use crate::pipeline::RagPipeline;

/// A retrieval tool that wraps a [`RagPipeline`] for agentic use.
///
/// Implements [`adk_core::Tool`] so it can be attached to any ADK agent.
/// The tool accepts a required `query` string and optional `collection`
/// and `top_k` parameters.
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
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdkError::tool("missing required 'query' parameter"))?;

        let collection =
            args.get("collection").and_then(|v| v.as_str()).unwrap_or(&self.default_collection);

        let top_k_override = args.get("top_k").and_then(|v| v.as_u64()).map(|v| v as usize);

        info!(query, collection, top_k_override, "rag_search tool called");

        let results = if let Some(top_k) = top_k_override {
            // Override top_k: embed query, search with custom top_k, rerank, filter
            self.query_with_top_k(collection, query, top_k).await
        } else {
            self.pipeline.query(collection, query).await
        };

        let results = results.map_err(|e| {
            error!(error = %e, "rag_search failed");
            AdkError::tool(format!("RAG search failed: {e}"))
        })?;

        serde_json::to_value(&results).map_err(|e| {
            error!(error = %e, "failed to serialize search results");
            AdkError::tool(format!("failed to serialize results: {e}"))
        })
    }
}

impl RagTool {
    /// Query with a custom `top_k`, bypassing the pipeline's configured value.
    async fn query_with_top_k(
        &self,
        collection: &str,
        query: &str,
        top_k: usize,
    ) -> crate::error::Result<Vec<crate::document::SearchResult>> {
        // 1. Embed the query
        let query_embedding =
            self.pipeline.embedding_provider().embed(query).await.map_err(|e| {
                crate::error::RagError::PipelineError(format!("query embedding failed: {e}"))
            })?;

        // 2. Search with the overridden top_k
        let results = self
            .pipeline
            .vector_store()
            .search(collection, &query_embedding, top_k)
            .await
            .map_err(|e| {
                crate::error::RagError::PipelineError(format!(
                    "search failed in collection '{collection}': {e}"
                ))
            })?;

        // 3. Filter by similarity threshold
        let threshold = self.pipeline.config().similarity_threshold;
        let filtered = results.into_iter().filter(|r| r.score >= threshold).collect();

        Ok(filtered)
    }
}
