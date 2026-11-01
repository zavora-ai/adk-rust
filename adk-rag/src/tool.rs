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
//! // [{ "id": "...", "document_id": "...", "text": "...", "metadata": {...},
//! //    "source_uri": "https://...", "title": "...", "score": 0.83 }]
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
use crate::filter::{Field, Filter};
use crate::pipeline::{QueryOptions, RagPipeline};
use crate::scope::Scope;

/// Resolves the scope a `rag_search` call searches from the call's context.
type ScopeResolver = dyn Fn(&dyn ToolContext) -> adk_core::Result<Scope> + Send + Sync;

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
/// `document_id`, `text`, `metadata`, and `score`, plus the chunk's `source_uri` and
/// `title` when its document had them, so the model can cite the source. Embedding
/// vectors stay out of the result: they mean nothing to a model and would fill its
/// context.
///
/// Each call searches one [`Scope`]: [`Scope::single_tenant`] unless
/// [`RagTool::with_scope`] fixes another or [`RagTool::with_scope_from`] resolves it from the
/// call's context. The model cannot name a scope. A policy filter
/// ([`RagTool::with_policy_filter`]) applies to every call, and the model may narrow results
/// further only on the metadata keys [`RagTool::with_model_filterable`] lists.
///
/// The tool is read-only and concurrency-safe, so `DeclarativePolicy::allow_read_only`
/// admits it and `ToolExecutionStrategy::Auto` overlaps it with adjacent read-only calls.
pub struct RagTool {
    pipeline: Arc<RagPipeline>,
    scope: Arc<ScopeResolver>,
    default_collection: String,
    /// Collections a call may name besides the default, in schema order.
    other_collections: Vec<String>,
    /// Applied to every call, beside any filter the model sends.
    policy_filter: Option<Filter>,
    /// Metadata keys the model may filter on, in schema order.
    model_filterable: Vec<String>,
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
        Self {
            pipeline,
            scope: Arc::new(|_: &dyn ToolContext| Ok(Scope::single_tenant())),
            default_collection: default_collection.into(),
            other_collections: vec![],
            policy_filter: None,
            model_filterable: vec![],
        }
    }

    /// Searches `scope` instead of [`Scope::single_tenant`].
    ///
    /// The scope is configuration, never a model argument: build the tool with the
    /// authenticated tenant's scope.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use std::sync::Arc;
    /// use adk_rag::{RagTool, Scope};
    ///
    /// let tool = RagTool::new(pipeline, "kb").with_scope(Scope::from_parts(["support", tenant_id])?);
    /// ```
    #[must_use]
    pub fn with_scope(mut self, scope: Scope) -> Self {
        self.scope = Arc::new(move |_: &dyn ToolContext| Ok(scope.clone()));
        self
    }

    /// Resolves each call's scope from its context, such as the authenticated user, instead
    /// of using one fixed scope.
    ///
    /// The resolver sees the call's [`ToolContext`] — app, user, session, state, identity —
    /// and never the model's arguments. An error fails the call before the vector store is
    /// queried.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_rag::{RagTool, Scope};
    ///
    /// // Each user searches the documents ingested under their organization's scope.
    /// let tool = RagTool::new(pipeline, "kb").with_scope_from(|ctx| {
    ///     let org = ctx.state().get("app:org").and_then(|v| v.as_str().map(str::to_string));
    ///     let org = org.ok_or_else(|| adk_core::AdkError::tool("no organization in state"))?;
    ///     Ok(Scope::from_parts(["support", org.as_str()])?)
    /// });
    /// ```
    #[must_use]
    pub fn with_scope_from<F>(mut self, resolve: F) -> Self
    where
        F: Fn(&dyn ToolContext) -> adk_core::Result<Scope> + Send + Sync + 'static,
    {
        self.scope = Arc::new(resolve);
        self
    }

    /// Searches a scope per user: `Scope::from_parts([app_name, user_id])` of each call.
    ///
    /// Ingest each user's documents under the same scope.
    #[must_use]
    pub fn with_scope_per_user(self) -> Self {
        self.with_scope_from(|ctx| Ok(Scope::from_parts([ctx.app_name(), ctx.user_id()])?))
    }

    /// Applies `filter` to every call, AND-ed with any filter the model sends.
    ///
    /// The policy filter is configuration, such as `visibility = public` for an anonymous
    /// audience; the model can narrow its results but never widen them past it. Its metadata
    /// keys must be declared filterable on the collections the tool searches.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_rag::{Field, Filter, RagTool};
    ///
    /// let tool = RagTool::new(pipeline, "kb")
    ///     .with_policy_filter(Filter::eq(Field::metadata("visibility"), "public"));
    /// ```
    #[must_use]
    pub fn with_policy_filter(mut self, filter: Filter) -> Self {
        self.policy_filter = Some(filter);
        self
    }

    /// Lets the model filter on these metadata keys with a `filter` argument.
    ///
    /// The argument maps each key to a string (equal) or an array of strings (one of), and
    /// every entry must match. A key outside this list fails the call with
    /// `tool.invalid_arguments`. Each key must be declared filterable on the collections the
    /// tool searches.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // The model may send {"query": "...", "filter": {"section": ["billing", "refunds"]}}.
    /// let tool = RagTool::new(pipeline, "kb").with_model_filterable(["section", "lang"]);
    /// ```
    #[must_use]
    pub fn with_model_filterable<I, S>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.model_filterable.clear();
        for key in keys.into_iter().map(Into::into) {
            if !self.model_filterable.contains(&key) {
                self.model_filterable.push(key);
            }
        }
        self
    }

    /// The filter the model's `filter` argument asks for, limited to model-filterable keys.
    fn model_filter(&self, argument: Option<&Value>) -> adk_core::Result<Option<Filter>> {
        let entries = match argument {
            None | Some(Value::Null) => return Ok(None),
            Some(Value::Object(entries)) if entries.is_empty() => return Ok(None),
            Some(Value::Object(entries)) => entries,
            Some(_) => return Err(invalid_arguments("'filter' must be an object")),
        };
        let allowed = self.model_filterable.join("', '");
        let mut filters = Vec::with_capacity(entries.len());
        for (key, value) in entries {
            if !self.model_filterable.contains(key) {
                return Err(invalid_arguments(&format!(
                    "'filter' cannot use '{key}'; the filterable keys are '{allowed}'"
                )));
            }
            let field = Field::metadata(key.clone());
            filters.push(match value {
                Value::String(value) => Filter::eq(field, value.clone()),
                Value::Array(values) if !values.is_empty() => {
                    let values = values
                        .iter()
                        .map(|value| value.as_str().map(str::to_string))
                        .collect::<Option<Vec<_>>>()
                        .ok_or_else(|| {
                            invalid_arguments(&format!("'filter.{key}' must hold strings"))
                        })?;
                    Filter::is_in(field, values)
                }
                _ => {
                    return Err(invalid_arguments(&format!(
                        "'filter.{key}' must be a string or a non-empty array of strings"
                    )));
                }
            });
        }
        Ok(Some(Filter::and(filters)))
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
        if !self.model_filterable.is_empty() {
            let mut keys = serde_json::Map::new();
            for key in &self.model_filterable {
                keys.insert(
                    key.clone(),
                    json!({
                        "anyOf": [
                            { "type": "string" },
                            { "type": "array", "items": { "type": "string" }, "minItems": 1 }
                        ]
                    }),
                );
            }
            properties["filter"] = json!({
                "type": "object",
                "properties": keys,
                "additionalProperties": false,
                "description": "Only return results whose metadata matches every entry: a string \
                                must equal the value, an array must contain it."
            });
        }
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

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
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

        let model_filter = self.model_filter(args.get("filter"))?;
        let scope = (self.scope)(ctx.as_ref())?;
        info!(query, collection, top_k, scope = scope.as_str(), "rag_search tool called");

        let mut options = QueryOptions::new().with_top_k(top_k);
        let filters: Vec<Filter> = self.policy_filter.iter().cloned().chain(model_filter).collect();
        if !filters.is_empty() {
            options = options.with_filter(Filter::and(filters));
        }
        let results =
            self.pipeline.scoped(scope).query_with(collection, query, &options).await.map_err(
                |e| {
                    error!(error = %e, "rag_search failed");
                    AdkError::tool(format!("RAG search failed: {e}"))
                },
            )?;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    source_uri: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
    score: f32,
}

impl<'a> From<&'a SearchResult> for RetrievedChunk<'a> {
    fn from(result: &'a SearchResult) -> Self {
        Self {
            id: &result.chunk.id,
            document_id: &result.chunk.document_id,
            text: &result.chunk.text,
            metadata: &result.chunk.metadata,
            source_uri: result.chunk.source_uri.as_deref(),
            title: result.chunk.title.as_deref(),
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
    /// "faq", and "tenant_b" holds "secret", all in the single-tenant scope; "kb" also holds
    /// another tenant's document "a".
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
            let mut document = Document::new(id, text).with_metadata("source", format!("{id}.md"));
            if id == "a" {
                document = document.with_source_uri("https://example.com/a").with_title("Returns");
            }
            pipeline
                .scoped(Scope::single_tenant())
                .ingest(collection, &document)
                .await
                .expect("ingest");
        }
        // Another tenant's document in the shared "kb" collection, under the same ID as "a".
        pipeline
            .scoped(other_tenant())
            .ingest("kb", &Document::new("a", "returns for the other tenant"))
            .await
            .expect("ingest");
        pipeline
    }

    fn other_tenant() -> Scope {
        Scope::from_parts(["kb-test", "other"]).expect("valid scope")
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
            if item["document_id"] == "a" {
                assert_eq!(
                    keys,
                    ["document_id", "id", "metadata", "score", "source_uri", "text", "title"]
                );
            } else {
                assert_eq!(keys, ["document_id", "id", "metadata", "score", "text"]);
            }
        }
        let first = &results[0];
        assert_eq!(
            first["metadata"]["source"],
            format!("{}.md", first["document_id"].as_str().unwrap())
        );
        assert!(!result.to_string().contains("embedding"));
    }

    #[tokio::test]
    async fn results_carry_the_source_uri_and_title_of_documents_that_have_them() {
        let result = tool(None)
            .await
            .execute(context(), json!({"query": "returns", "top_k": 3}))
            .await
            .expect("search");

        let mut provenance: Vec<(String, Value, Value)> = result
            .as_array()
            .expect("an array of results")
            .iter()
            .map(|item| {
                let id = item["document_id"].as_str().expect("document_id").to_string();
                let field = |name: &str| item.get(name).cloned().unwrap_or(Value::Null);
                (id, field("source_uri"), field("title"))
            })
            .collect();
        provenance.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            provenance,
            [
                ("a".to_string(), json!("https://example.com/a"), json!("Returns")),
                ("b".to_string(), Value::Null, Value::Null),
                ("c".to_string(), Value::Null, Value::Null),
            ]
        );
    }

    #[tokio::test]
    async fn the_tool_searches_only_its_configured_scope() {
        let pipeline = pipeline(None).await;
        let texts = |result: Value| -> Vec<String> {
            let mut texts: Vec<String> = result
                .as_array()
                .expect("an array of results")
                .iter()
                .map(|item| item["text"].as_str().expect("text").to_string())
                .collect();
            texts.sort();
            texts
        };

        let single = RagTool::new(pipeline.clone(), "kb");
        let result = single
            .execute(context(), json!({"query": "returns", "top_k": 10}))
            .await
            .expect("search");
        assert_eq!(texts(result), ["returns within thirty days", "ships in two days", "warranty"]);

        // A scope argument from the model is not a parameter and changes nothing.
        let other = RagTool::new(pipeline, "kb").with_scope(other_tenant());
        let result = other
            .execute(context(), json!({"query": "returns", "top_k": 10, "scope": ":single-tenant"}))
            .await
            .expect("search");
        assert_eq!(texts(result), ["returns for the other tenant"]);
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

    #[tokio::test]
    async fn a_scope_resolved_from_the_context_never_from_arguments() {
        let pipeline = pipeline(None).await;
        let user = Scope::from_parts(["rag-tool-test", "anonymous"]).expect("scope");
        pipeline
            .scoped(user)
            .ingest("kb", &Document::new("mine", "returns of this user"))
            .await
            .expect("ingest");

        let tool = RagTool::new(pipeline.clone(), "kb").with_scope_per_user();
        let result = tool
            .execute(context(), json!({"query": "returns", "top_k": 10, "scope": "x"}))
            .await
            .expect("search");
        assert_eq!(document_ids(&result), ["mine"]);

        let refusing = RagTool::new(pipeline, "kb")
            .with_scope_from(|_| Err(AdkError::tool("no tenant on this request")));
        let error =
            refusing.execute(context(), json!({"query": "returns"})).await.expect_err("refused");
        assert!(error.message.contains("no tenant"), "{error}");
    }

    async fn filtered_tool() -> RagTool {
        let pipeline = pipeline(None).await;
        pipeline
            .create_collection_with_filterable("policies", ["visibility", "section"])
            .await
            .expect("create");
        let rag = pipeline.scoped(Scope::single_tenant());
        for (id, visibility, section) in [
            ("public_billing", "public", "billing"),
            ("public_refunds", "public", "refunds"),
            ("private_billing", "private", "billing"),
        ] {
            let document = Document::new(id, format!("returns {id}"))
                .with_metadata("visibility", visibility)
                .with_metadata("section", section);
            rag.ingest("policies", &document).await.expect("ingest");
        }
        RagTool::new(pipeline, "policies")
            .with_policy_filter(Filter::eq(Field::metadata("visibility"), "public"))
            .with_model_filterable(["section"])
    }

    #[tokio::test]
    async fn the_policy_filter_always_applies_and_the_model_may_only_narrow() {
        let tool = filtered_tool().await;
        let search = |args: Value| tool.execute(context(), args);

        let all = search(json!({"query": "returns", "top_k": 10})).await.expect("search");
        assert_eq!(document_ids(&all), ["public_billing", "public_refunds"]);
        let billing = search(json!({"query": "returns", "filter": {"section": "billing"}}))
            .await
            .expect("search");
        assert_eq!(document_ids(&billing), ["public_billing"]);
        let either = search(json!({"query": "returns", "filter": {"section": ["refunds", "x"]}}))
            .await
            .expect("search");
        assert_eq!(document_ids(&either), ["public_refunds"]);

        for filter in [
            json!({"visibility": "private"}),
            json!({"section": 3}),
            json!({"section": []}),
            json!("section=billing"),
        ] {
            let error = search(json!({"query": "returns", "filter": filter}))
                .await
                .expect_err("invalid filter");
            assert_eq!(error.code, TOOL_INVALID_ARGUMENTS_CODE, "{filter}");
        }

        let schema = tool.parameters_schema().expect("schema");
        let keys: Vec<&String> = schema["properties"]["filter"]["properties"]
            .as_object()
            .expect("keys")
            .keys()
            .collect();
        assert_eq!(keys, ["section"]);
    }
}
